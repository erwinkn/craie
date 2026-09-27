//! The hit-test index (E15): each node's reach, a box around everything a
//! hit test can find in its subtree, in its parent's child frame (the
//! frame `hit_node` receives its point in). A hit test skips a subtree
//! whose reach misses the point, so it visits the nodes on the way down
//! and their siblings, not the whole tree.
//!
//! reach(n) = n's box ∪ (its children's reaches, less n's scroll offset,
//! cut to n's clip on its clipped axes), mapped through n's transform and
//! placed at n's origin; empty under `display: none`.
//!
//! Reach is kept lazily. A change to a node's box, clip, overflow,
//! display, scroll offset, transform or children marks it stale, with its
//! ancestors up to the first one already stale (`Host::touch`: layout,
//! `set_layout`, `set_spatial`, `scroll_to` and the tree edits call it).
//! `refresh_reach` recomputes the stale nodes, children first: at the end
//! of a frame's layout (`Ui::render`), and again before an event is
//! dispatched, for what changed since (a transaction, a scroll). A hit
//! test between a change and the refresh walks stale nodes in full.
//!
//! Each reach is padded against rounding by more than the hit test's own
//! arithmetic can differ by, scaled by the condition number of the node's
//! transform (`Bounds::padded`), so that `hit_test` and `hit_test_walk`
//! agree. The pad is an error estimate, not a proof: the tests below
//! check the agreement at points aimed at box edges and corners, under
//! transforms that squash one axis up to 100,000-fold (a pad without
//! the condition number fails there).

use craie_core::geom::{Affine, Point};

use crate::host::{NodeId, ROOT};
use crate::ui::Ui;

/// A closed axis-aligned box by its corners; empty when a min passes
/// its max.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bounds {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Bounds {
    pub const EMPTY: Bounds = Bounds {
        x0: f32::INFINITY,
        y0: f32::INFINITY,
        x1: f32::NEG_INFINITY,
        y1: f32::NEG_INFINITY,
    };

    fn size(width: f32, height: f32) -> Bounds {
        Bounds {
            x0: 0.0,
            y0: 0.0,
            x1: width,
            y1: height,
        }
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }

    fn is_empty(&self) -> bool {
        !(self.x0 <= self.x1 && self.y0 <= self.y1)
    }

    fn union(self, o: Bounds) -> Bounds {
        Bounds {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    fn offset(self, dx: f32, dy: f32) -> Bounds {
        Bounds {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
        }
    }

    /// The box around the four mapped corners.
    fn map(self, m: &Affine) -> Bounds {
        let corners = [
            Point::new(self.x0, self.y0),
            Point::new(self.x1, self.y0),
            Point::new(self.x0, self.y1),
            Point::new(self.x1, self.y1),
        ];
        corners.iter().fold(Bounds::EMPTY, |b, &c| {
            let p = m.apply(c);
            b.union(Bounds {
                x0: p.x,
                y0: p.y,
                x1: p.x,
                y1: p.y,
            })
        })
    }

    /// Grown by more than the rounding the hit test's own arithmetic
    /// (inverse transforms, origin and scroll offsets) can differ by:
    /// a few ulps at the box's magnitude, times `k`, the condition
    /// number of the node's transform (1 without one, or for rotations
    /// and uniform scales), since inverting a transform that squashes
    /// one axis multiplies its rounding.
    fn padded(self, k: f32) -> Bounds {
        let m = self
            .x0
            .abs()
            .max(self.x1.abs())
            .max(self.y0.abs())
            .max(self.y1.abs());
        let e = 1e-3 + 4e-6 * m * k;
        Bounds {
            x0: self.x0 - e,
            y0: self.y0 - e,
            x1: self.x1 + e,
            y1: self.y1 + e,
        }
    }
}

impl Ui {
    /// Recomputes the reach of every stale node. `render` calls it once
    /// the frame's geometry is final, and `dispatch` first thing, for
    /// changes since the frame; with nothing stale it only checks the
    /// roots.
    pub fn refresh_reach(&mut self) {
        let slots = self.host.slot_count();
        if self.reach.len() < slots {
            self.reach.resize(slots, Bounds::EMPTY);
        }
        for i in 0..self.host.child_count(ROOT) {
            let root = self.host.child_at(ROOT, i);
            if self.host.reach_stale(root) {
                self.refresh_node(root);
            }
        }
    }

    /// Children first: a stale node's stale children are refreshed
    /// before their reaches are gathered.
    fn refresh_node(&mut self, id: NodeId) {
        let mut kids = Bounds::EMPTY;
        for i in 0..self.host.child_count(id) {
            let c = self.host.child_at(id, i);
            if !self.host.is_live(c) {
                continue;
            }
            if self.host.reach_stale(c) {
                self.refresh_node(c);
            }
            kids = kids.union(self.reach[c.index()]);
        }
        self.reach[id.index()] = self.reach_of(id, kids);
        self.host.reach_fresh(id);
    }

    /// `id`'s reach from its children's (`kids`, in its child frame).
    fn reach_of(&self, id: NodeId, kids: Bounds) -> Bounds {
        let style = self.host.style(id);
        if style.display() == taffy::Display::None {
            return Bounds::EMPTY;
        }
        let data = self.layouts.data(id);
        let size = data.rect.size;
        let mut reach = Bounds::size(size.width, size.height);
        if !kids.is_empty() {
            let [sx, sy] = self.scroll_offset_if_scrolls(id);
            let mut kids = kids.offset(-sx, -sy);
            let (clip, _, open) = self.clip_shape(id, [0.0, 0.0], &data);
            if !open[0] {
                kids.x0 = kids.x0.max(clip.origin.x);
                kids.x1 = kids.x1.min(clip.max_x());
            }
            if !open[1] {
                kids.y0 = kids.y0.max(clip.origin.y);
                kids.y1 = kids.y1.min(clip.max_y());
            }
            if !kids.is_empty() {
                reach = reach.union(kids);
            }
        }
        let t = self.host.spatial[id.index()].transform;
        let mut k = 1.0;
        if t != Affine::IDENTITY {
            let det = t.determinant();
            if det == 0.0 || !det.is_finite() {
                // No inverse: `hit_node` finds nothing here.
                return Bounds::EMPTY;
            }
            // ‖A‖²/(2|det A|), the Frobenius condition number over 2.
            let [a, b, c, d, _, _] = t.0;
            k = ((a * a + b * b + c * c + d * d) / (2.0 * det.abs())).max(1.0);
            reach = reach.map(&t.about(Point::new(size.width / 2.0, size.height / 2.0)));
        }
        reach
            .offset(data.rect.origin.x, data.rect.origin.y)
            .padded(k)
    }
}

#[cfg(test)]
mod tests {
    use craie_core::geom::{Affine, Point, Size};
    use craie_core::rng::Rng;

    use crate::events::Event;
    use crate::host::NodeId;
    use crate::mutation::{NIL, NodeKind, Transaction};
    use crate::ui::Ui;

    const VIEW: Size = Size {
        width: 800.0,
        height: 600.0,
    };

    fn overflow(rng: &mut Rng) -> taffy::Overflow {
        match rng.below(6) {
            0 => taffy::Overflow::Hidden,
            1 => taffy::Overflow::Scroll,
            2 => taffy::Overflow::Clip,
            _ => taffy::Overflow::Visible,
        }
    }

    /// A border or padding width: fixed, or a percentage of the
    /// parent's width (so a parent's resize moves a child's clip
    /// without resizing the child).
    fn edge(rng: &mut Rng) -> taffy::LengthPercentage {
        match rng.below(3) {
            0 => taffy::LengthPercentage::length(0.0),
            1 => taffy::LengthPercentage::length(rng.unit() * 16.0),
            _ => taffy::LengthPercentage::percent(rng.unit() * 0.25),
        }
    }

    fn edges(rng: &mut Rng) -> taffy::Rect<taffy::LengthPercentage> {
        taffy::Rect {
            left: edge(rng),
            right: edge(rng),
            top: edge(rng),
            bottom: edge(rng),
        }
    }

    /// A random box: flex or absolutely placed (often past its
    /// parent), with borders and padding, clipping or scrolling on
    /// either axis, sometimes hidden.
    fn style(rng: &mut Rng) -> taffy::Style {
        let len = |rng: &mut Rng, max: f32| taffy::Dimension::length(4.0 + rng.unit() * max);
        let mut s = taffy::Style {
            size: taffy::Size {
                width: len(rng, 300.0),
                height: len(rng, 300.0),
            },
            border: edges(rng),
            padding: edges(rng),
            flex_shrink: 0.0,
            flex_wrap: taffy::FlexWrap::Wrap,
            overflow: taffy::Point {
                x: overflow(rng),
                y: overflow(rng),
            },
            ..taffy::Style::default()
        };
        if rng.chance(0.3) {
            let at =
                |rng: &mut Rng| taffy::LengthPercentageAuto::length(rng.unit() * 500.0 - 150.0);
            s.position = taffy::Position::Absolute;
            s.inset = taffy::Rect {
                left: at(rng),
                top: at(rng),
                ..taffy::Rect::auto()
            };
        }
        if rng.chance(0.05) {
            s.display = taffy::Display::None;
        }
        s
    }

    /// Mostly ordinary transforms, and a few ill-conditioned ones (one
    /// axis squashed a thousandfold), whose inverse rounds the most.
    fn transform(rng: &mut Rng) -> Affine {
        match rng.below(7) {
            0 => Affine::rotate(rng.unit() * 6.3),
            1 => Affine::scale(0.2 + rng.unit() * 2.0, 0.2 + rng.unit() * 2.0),
            2 => Affine::translate(rng.unit() * 200.0 - 100.0, rng.unit() * 200.0 - 100.0),
            3 => Affine::rotate(rng.unit()).mul(&Affine::scale(1.5, 0.5)),
            4 => Affine::rotate(rng.unit() * 6.3).mul(&Affine::scale(0.001, 1.0)),
            5 => Affine::rotate(rng.unit() * 6.3).mul(&Affine::scale(3.0, 0.003)),
            _ => Affine::IDENTITY,
        }
    }

    /// Creates a node under a random live node (or as a root), on an id
    /// freed earlier if there is one, as the bridge recycles them.
    fn spawn(
        t: &mut Transaction,
        rng: &mut Rng,
        live: &mut Vec<u32>,
        free: &mut Vec<u32>,
        next: &mut u32,
    ) {
        let id = free.pop().unwrap_or_else(|| {
            *next += 1;
            *next - 1
        });
        let parent = if live.is_empty() || rng.chance(0.05) {
            NIL
        } else {
            live[rng.below(live.len() as u32) as usize]
        };
        t.create(id, NodeKind::View).layout(id, &style(rng));
        if rng.chance(0.3) {
            t.transform(id, transform(rng));
        }
        if rng.chance(0.2) {
            t.paint(id, None, Some(rng.unit() * 40.0), None);
        }
        t.append(parent, id);
        live.push(id);
    }

    /// Whether `a` is `b` or one of its ancestors.
    fn contains(ui: &Ui, a: u32, b: u32) -> bool {
        ui.ancestors(NodeId(b)).any(|n| n.0 == a)
    }

    /// The pruned hit test answers as the full walk, with the index
    /// fresh and between a change and its refresh, as trees are built,
    /// restyled (borders and padding alone, too), transformed,
    /// scrolled, moved and pruned, with removed ids reused at once.
    #[test]
    fn index_agrees_with_walk() {
        for seed in 1..=12u64 {
            let mut rng = Rng::new(seed * 0x9E37);
            let mut ui = Ui::new(1.0);
            let mut live: Vec<u32> = Vec::new();
            let mut free: Vec<u32> = Vec::new();
            let mut next = 0u32;
            for round in 0..24 {
                let mut t = Transaction::new(round + 1);
                // Grow.
                for _ in 0..rng.below(24) {
                    spawn(&mut t, &mut rng, &mut live, &mut free, &mut next);
                }
                ui.apply_txn(&t).unwrap();
                // Change, one edit per transaction (a move checks the
                // tree it applies to for cycles).
                for _ in 0..rng.below(12) {
                    if live.is_empty() {
                        break;
                    }
                    let mut t = Transaction::new(round + 1);
                    let id = live[rng.below(live.len() as u32) as usize];
                    match rng.below(6) {
                        0 => {
                            t.layout(id, &style(&mut rng));
                        }
                        1 => {
                            // Borders and padding alone, or the width
                            // alone (its children's percentage borders
                            // move their clips, not their boxes).
                            let mut s = ui.host.style(NodeId(id)).to_taffy();
                            if rng.chance(0.5) {
                                s.border = edges(&mut rng);
                                s.padding = edges(&mut rng);
                            } else {
                                s.size.width = taffy::Dimension::length(4.0 + rng.unit() * 300.0);
                            }
                            t.layout(id, &s);
                        }
                        2 => {
                            t.transform(id, transform(&mut rng));
                        }
                        3 => {
                            let to = live[rng.below(live.len() as u32) as usize];
                            if !contains(&ui, id, to) {
                                t.append(to, id);
                            }
                        }
                        4 => {
                            // The slot goes straight to a new node.
                            t.remove(id);
                            live.retain(|&n| n != id);
                            free.push(id);
                            spawn(&mut t, &mut rng, &mut live, &mut free, &mut next);
                        }
                        _ => {
                            t.append(NIL, id);
                        }
                    }
                    ui.apply_txn(&t).unwrap();
                }
                check(&ui, &mut rng, &live, "applied, before layout");
                ui.layout(VIEW);
                check(&ui, &mut rng, &live, "laid out, stale");
                ui.render(VIEW);
                assert!(fresh(&ui, &live), "a frame refreshes every attached node");
                check(&ui, &mut rng, &live, "rendered, fresh");
                // Scroll twice: from a fresh tree, then from one a
                // dispatch refreshed.
                for pass in 0..2 {
                    for &id in &live {
                        if rng.chance(0.2) {
                            let (x, y) = (rng.unit() * 400.0, rng.unit() * 400.0);
                            ui.scroll_to(NodeId(id), x, y);
                        }
                    }
                    check(&ui, &mut rng, &live, &format!("scrolled ({pass}), stale"));
                    ui.dispatch(&Event::PointerMove { x: 1.0, y: 1.0 });
                    assert!(
                        fresh(&ui, &live),
                        "a dispatch refreshes every attached node"
                    );
                    check(&ui, &mut rng, &live, &format!("scrolled ({pass}), fresh"));
                }
            }
        }
    }

    /// A clip that moves inside a box that does not: C's bottom border
    /// is a percentage of its parent's width, so narrowing the parent
    /// lowers C's clip (C clips y only) until it reaches C's child G,
    /// which sticks out to the right. G's part of C's reach goes from
    /// nothing to the box right of C. Only the layout pass sees it.
    #[test]
    fn index_follows_a_clip_that_moves_alone() {
        let px = |v: f32| taffy::Dimension::length(v);
        let parent = |w: f32| taffy::Style {
            size: taffy::Size {
                width: px(w),
                height: px(200.0),
            },
            ..taffy::Style::default()
        };
        let c = taffy::Style {
            size: taffy::Size {
                width: px(100.0),
                height: px(100.0),
            },
            border: taffy::Rect {
                bottom: taffy::LengthPercentage::percent(0.1),
                ..taffy::Rect::zero()
            },
            overflow: taffy::Point {
                x: taffy::Overflow::Visible,
                y: taffy::Overflow::Hidden,
            },
            ..taffy::Style::default()
        };
        let g = taffy::Style {
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(100.0),
                top: taffy::LengthPercentageAuto::length(70.0),
                ..taffy::Rect::auto()
            },
            size: taffy::Size {
                width: px(100.0),
                height: px(30.0),
            },
            ..taffy::Style::default()
        };
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View)
            .layout(0, &parent(400.0))
            .append(NIL, 0);
        t.create(1, NodeKind::View).layout(1, &c).append(0, 1);
        t.create(2, NodeKind::View).layout(2, &g).append(1, 2);
        let mut ui = Ui::new(1.0);
        ui.apply_txn(&t).unwrap();
        ui.render(VIEW);
        // A 40 pt border: C clips above y = 60, and G starts at 70.
        assert_eq!(ui.hit_test(150.0, 75.0), Some(NodeId(0)));
        let mut t = Transaction::new(2);
        t.layout(0, &parent(200.0));
        ui.apply_txn(&t).unwrap();
        ui.layout(VIEW);
        // 20 pt: C clips above y = 80, and G shows from 70 to 80.
        assert_eq!(ui.hit_test_walk(150.0, 75.0), Some(NodeId(2)));
        assert_eq!(ui.hit_test(150.0, 75.0), Some(NodeId(2)), "laid out");
        ui.render(VIEW);
        assert_eq!(ui.hit_test(150.0, 75.0), Some(NodeId(2)), "rendered");
    }

    /// Whether every attached node's reach is fresh.
    fn fresh(ui: &Ui, live: &[u32]) -> bool {
        live.iter()
            .all(|&id| !ui.host.reach_stale(NodeId(id)) || !attached(ui, id))
    }

    /// Whether `id` is in the tree (its top ancestor is a root).
    fn attached(ui: &Ui, id: u32) -> bool {
        let top = ui.ancestors(NodeId(id)).last().unwrap();
        ui.host.parent(top) == NodeId::NIL
    }

    /// Index and walk agree at random points, and at points aimed at
    /// random nodes' edges and corners (where rounding decides).
    fn check(ui: &Ui, rng: &mut Rng, live: &[u32], when: &str) {
        for i in 0..400 {
            let (x, y) = if i % 2 == 0 || live.is_empty() {
                (rng.unit() * 1000.0 - 100.0, rng.unit() * 800.0 - 100.0)
            } else {
                let id = NodeId(live[rng.below(live.len() as u32) as usize]);
                let p = near_edge(rng, &ui.node_to_window(id), ui.layouts.data(id).rect.size);
                (p.x, p.y)
            };
            assert_eq!(
                ui.hit_test(x, y),
                ui.hit_test_walk(x, y),
                "at ({x}, {y}), {when}"
            );
        }
    }

    /// A window point just inside or outside the edge of a `size` box
    /// that `m` places: off an edge in the box's frame, or off a corner
    /// in the window's (a corner is where the box meets its reach), by
    /// 0.1 down to 1e-7.
    fn near_edge(rng: &mut Rng, m: &Affine, size: Size) -> Point {
        let (w, h) = (size.width, size.height);
        let d = 10f32.powi(-1 - rng.below(7) as i32) * (1.0 + 9.0 * rng.unit());
        let u = rng.unit();
        if rng.chance(0.5) {
            let d = if rng.chance(0.5) { d } else { -d };
            m.apply(match rng.below(4) {
                0 => Point::new(u * w, -d),
                1 => Point::new(u * w, h + d),
                2 => Point::new(-d, u * h),
                _ => Point::new(w + d, u * h),
            })
        } else {
            let corner = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)][rng.below(4) as usize];
            let c = m.apply(Point::new(corner.0, corner.1));
            let a = u * std::f32::consts::TAU;
            Point::new(c.x + d * a.cos(), c.y + d * a.sin())
        }
    }

    /// Index and walk agree around a box under an ill-conditioned
    /// transform (one axis squashed up to 100,000-fold, turned both
    /// sides), a million points aimed at its edges and corners. A pad
    /// that ignores the transform's conditioning misses a few of them.
    #[test]
    fn index_agrees_under_squashing_transforms() {
        let mut rng = Rng::new(7);
        let px = |v: f32| taffy::Dimension::length(v);
        let at = |v: f32| taffy::LengthPercentageAuto::length(v);
        for k in 0..200 {
            let (w, h) = (50.0 + rng.unit() * 500.0, 50.0 + rng.unit() * 500.0);
            let root = taffy::Style {
                size: taffy::Size {
                    width: px(VIEW.width),
                    height: px(VIEW.height),
                },
                ..taffy::Style::default()
            };
            let boxed = taffy::Style {
                position: taffy::Position::Absolute,
                inset: taffy::Rect {
                    left: at(rng.unit() * 600.0),
                    top: at(rng.unit() * 400.0),
                    ..taffy::Rect::auto()
                },
                size: taffy::Size {
                    width: px(w),
                    height: px(h),
                },
                ..taffy::Style::default()
            };
            let squash = [1e-2, 1e-3, 1e-4, 1e-5][k % 4];
            let mut t = Transaction::new(1);
            t.create(0, NodeKind::View).layout(0, &root).append(NIL, 0);
            t.create(1, NodeKind::View).layout(1, &boxed).append(0, 1);
            t.transform(
                1,
                Affine::rotate(rng.unit() * 6.3)
                    .mul(&Affine::scale(squash, 1.0))
                    .mul(&Affine::rotate(rng.unit() * 6.3)),
            );
            let mut ui = Ui::new(1.0);
            ui.apply_txn(&t).unwrap();
            ui.render(VIEW);
            let m = ui.node_to_window(NodeId(1));
            for _ in 0..5000 {
                let p = near_edge(&mut rng, &m, Size::new(w, h));
                assert_eq!(
                    ui.hit_test(p.x, p.y),
                    ui.hit_test_walk(p.x, p.y),
                    "at ({}, {}), squashed {squash}",
                    p.x,
                    p.y
                );
            }
        }
    }
}
