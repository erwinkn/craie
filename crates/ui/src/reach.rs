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
//! `refresh_reach` recomputes the stale nodes, children first, before an
//! event is dispatched. A hit test between a change and the refresh walks
//! stale nodes in full, so the index never changes an answer:
//! `hit_test` and `hit_test_walk` agree.

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
    /// a few ulps at the box's magnitude.
    fn padded(self) -> Bounds {
        let m = self
            .x0
            .abs()
            .max(self.x1.abs())
            .max(self.y0.abs())
            .max(self.y1.abs());
        let e = 1e-3 + 4e-6 * m;
        Bounds {
            x0: self.x0 - e,
            y0: self.y0 - e,
            x1: self.x1 + e,
            y1: self.y1 + e,
        }
    }
}

impl Ui {
    /// Recomputes the reach of every stale node. `dispatch` calls it
    /// first; a host may also call it after a frame's layout, to keep
    /// the work off the next event.
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
        if t != Affine::IDENTITY {
            reach = reach.map(&t.about(Point::new(size.width / 2.0, size.height / 2.0)));
        }
        reach
            .offset(data.rect.origin.x, data.rect.origin.y)
            .padded()
    }
}

#[cfg(test)]
mod tests {
    use craie_core::geom::{Affine, Size};
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

    /// A random box: flex or absolutely placed (often past its
    /// parent), clipping or scrolling on either axis, sometimes hidden.
    fn style(rng: &mut Rng) -> taffy::Style {
        let len = |rng: &mut Rng, max: f32| taffy::Dimension::length(4.0 + rng.unit() * max);
        let mut s = taffy::Style {
            size: taffy::Size {
                width: len(rng, 300.0),
                height: len(rng, 300.0),
            },
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

    fn transform(rng: &mut Rng) -> Affine {
        match rng.below(5) {
            0 => Affine::rotate(rng.unit() * 6.3),
            1 => Affine::scale(0.2 + rng.unit() * 2.0, 0.2 + rng.unit() * 2.0),
            2 => Affine::translate(rng.unit() * 200.0 - 100.0, rng.unit() * 200.0 - 100.0),
            3 => Affine::rotate(rng.unit()).mul(&Affine::scale(1.5, 0.5)),
            _ => Affine::IDENTITY,
        }
    }

    /// Whether `a` is `b` or one of its ancestors.
    fn contains(ui: &Ui, a: u32, b: u32) -> bool {
        ui.ancestors(NodeId(b)).any(|n| n.0 == a)
    }

    /// The pruned hit test answers as the full walk, with the index
    /// fresh and between a change and its refresh, as trees are built,
    /// restyled, transformed, scrolled, moved and pruned.
    #[test]
    fn index_agrees_with_walk() {
        for seed in 1..=12u64 {
            let mut rng = Rng::new(seed * 0x9E37);
            let mut ui = Ui::new(1.0);
            let mut live: Vec<u32> = Vec::new();
            let mut next = 0u32;
            for round in 0..24 {
                let mut t = Transaction::new(round + 1);
                // Grow.
                for _ in 0..rng.below(24) {
                    let id = next;
                    next += 1;
                    let parent = if live.is_empty() || rng.chance(0.05) {
                        NIL
                    } else {
                        live[rng.below(live.len() as u32) as usize]
                    };
                    t.create(id, NodeKind::View).layout(id, &style(&mut rng));
                    if rng.chance(0.3) {
                        t.transform(id, transform(&mut rng));
                    }
                    if rng.chance(0.2) {
                        t.paint(id, None, Some(rng.unit() * 40.0), None);
                    }
                    t.append(parent, id);
                    live.push(id);
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
                    match rng.below(5) {
                        0 => {
                            t.layout(id, &style(&mut rng));
                        }
                        1 => {
                            t.transform(id, transform(&mut rng));
                        }
                        2 => {
                            let to = live[rng.below(live.len() as u32) as usize];
                            if !contains(&ui, id, to) {
                                t.append(to, id);
                            }
                        }
                        3 => {
                            t.remove(id);
                            live.retain(|&n| n != id);
                        }
                        _ => {
                            t.append(NIL, id);
                        }
                    }
                    ui.apply_txn(&t).unwrap();
                }
                check(&ui, &mut rng, "applied, before layout");
                ui.render(VIEW);
                check(&ui, &mut rng, "laid out, stale");
                // Scroll twice: from a laid-out (stale) tree, then from a
                // fresh one.
                for pass in 0..2 {
                    for &id in &live {
                        if rng.chance(0.2) {
                            let (x, y) = (rng.unit() * 400.0, rng.unit() * 400.0);
                            ui.scroll_to(NodeId(id), x, y);
                        }
                    }
                    check(&ui, &mut rng, &format!("scrolled ({pass}), stale"));
                    ui.dispatch(&Event::PointerMove { x: 1.0, y: 1.0 });
                    assert!(
                        live.iter()
                            .all(|&id| !ui.host.reach_stale(NodeId(id)) || !attached(&ui, id)),
                        "a dispatch refreshes every attached node"
                    );
                    check(&ui, &mut rng, &format!("scrolled ({pass}), fresh"));
                }
            }
        }
    }

    /// Whether `id` is in the tree (its top ancestor is a root).
    fn attached(ui: &Ui, id: u32) -> bool {
        let top = ui.ancestors(NodeId(id)).last().unwrap();
        ui.host.parent(top) == NodeId::NIL
    }

    fn check(ui: &Ui, rng: &mut Rng, when: &str) {
        for _ in 0..400 {
            let (x, y) = (rng.unit() * 1000.0 - 100.0, rng.unit() * 800.0 - 100.0);
            assert_eq!(
                ui.hit_test(x, y),
                ui.hit_test_walk(x, y),
                "at ({x}, {y}), {when}"
            );
        }
    }
}
