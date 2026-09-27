//! Paint order through `Ui`: what the scene draws, what a hit reaches,
//! what a z change costs, what keeps tree order, and layers.

use craie_core::rng::Rng;

use crate::events::{Button, Event, Key, KeyInput, Mods, mask};
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{NodeKind, Transaction};
use crate::ui::Ui;

const NIL: u32 = u32::MAX;
const VIEW: Size = Size {
    width: 400.0,
    height: 400.0,
};

/// A `w` × `h` box at (`x`, `y`) in its parent.
fn boxed(x: f32, y: f32, w: f32, h: f32) -> taffy::Style {
    taffy::Style {
        position: taffy::Position::Absolute,
        inset: taffy::Rect {
            left: taffy::LengthPercentageAuto::length(x),
            top: taffy::LengthPercentageAuto::length(y),
            ..taffy::Rect::auto()
        },
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    }
}

/// Creates view `id` in `parent`, filled with a color that names it.
fn view(t: &mut Transaction, parent: u32, id: u32, style: &taffy::Style) {
    t.create(id, NodeKind::View)
        .layout(id, style)
        .paint(id, Some(id << 8 | 0xFF), None, None)
        .append(parent, id);
}

/// The nodes the last frame drew, back to front, by their fill.
fn painted(ui: &mut Ui) -> Vec<u32> {
    ui.render(VIEW)
        .resolve(&|r| r.0 as u64)
        .iter()
        .filter(|p| p.kind == 0)
        .map(|p| p.color >> 8)
        .collect()
}

/// Root 0 (the back) holding `zs.len()` boxes 1, 2... over one
/// another, with those z.
fn stack(zs: &[i32]) -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    view(&mut t, NIL, 0, &boxed(0.0, 0.0, 200.0, 200.0));
    for (i, &z) in zs.iter().enumerate() {
        let id = i as u32 + 1;
        view(&mut t, 0, id, &boxed(0.0, 0.0, 100.0, 100.0));
        t.z(id, z);
    }
    ui.apply_txn(&t).unwrap();
    ui
}

/// Children of 0 stably sorted by z, as RN's zIndex orders them.
fn by_z(ui: &Ui) -> Vec<u32> {
    let mut kids: Vec<u32> = ui.host.children(NodeId(0)).iter().map(|c| c.0).collect();
    kids.sort_by_key(|&c| ui.host.spatial[c as usize].z);
    kids
}

/// Hits at one point, peeling off each node hit: front to back.
fn peel(ui: &mut Ui) -> Vec<u32> {
    let mut hits = Vec::new();
    let mut seq = 2;
    while let Some(id) = ui.hit_test(50.0, 50.0) {
        hits.push(id.0);
        let mut t = Transaction::new(seq);
        t.remove(id.0);
        ui.apply_txn(&t).unwrap();
        seq += 1;
    }
    hits
}

/// The scene draws each parent's children stably sorted by z; hits
/// reach them in reverse.
#[test]
fn paint_order_is_a_stable_sort_by_z() {
    for seed in 1..=20u64 {
        let mut rng = Rng::new(seed);
        let zs: Vec<i32> = (0..12).map(|_| rng.below(5) as i32 - 2).collect();
        let mut ui = stack(&zs);
        let order = by_z(&ui);
        let mut back_to_front = vec![0];
        back_to_front.extend(&order);
        assert_eq!(painted(&mut ui), back_to_front, "z {zs:?}");
        let mut front_to_back = back_to_front;
        front_to_back.reverse();
        assert_eq!(peel(&mut ui), front_to_back, "z {zs:?}");
    }
}

/// A parent whose children all have z = 0 holds no order; a z, a
/// child arriving with one, a move and a removal re-sort it, and it
/// drops the order once every z is back to 0.
#[test]
fn structure_and_z_changes_invalidate_the_order() {
    let mut ui = stack(&[0, 0, 0]);
    assert_eq!(painted(&mut ui), [0, 1, 2, 3]);
    assert!(ui.host.orders.is_empty(), "all z = 0: no order");
    let edit = |ui: &mut Ui, f: &dyn Fn(&mut Transaction)| {
        let mut t = Transaction::new(2);
        f(&mut t);
        ui.apply_txn(&t).unwrap();
        painted(ui)
    };
    assert_eq!(
        edit(&mut ui, &|t| {
            t.z(1, 1);
        }),
        [0, 2, 3, 1]
    );
    // 4 arrives in front of 1 with z 1: after 1 in tree order, so
    // above it among equals.
    let arrive = |t: &mut Transaction| {
        view(t, 0, 4, &boxed(0.0, 0.0, 100.0, 100.0));
        t.z(4, 1);
    };
    assert_eq!(edit(&mut ui, &arrive), [0, 2, 3, 1, 4]);
    // A move to the front of the tree order: 4 now precedes 1.
    assert_eq!(
        edit(&mut ui, &|t| {
            t.place(0, 4, 1);
        }),
        [0, 2, 3, 4, 1]
    );
    assert_eq!(
        edit(&mut ui, &|t| {
            t.remove(2);
        }),
        [0, 3, 4, 1]
    );
    assert_eq!(
        edit(&mut ui, &|t| {
            t.z(3, 2);
        }),
        [0, 4, 1, 3]
    );
    let zero = |t: &mut Transaction| {
        t.z(1, 0).z(3, 0).z(4, 0);
    };
    assert_eq!(edit(&mut ui, &zero), [0, 4, 1, 3]);
    assert!(ui.host.orders.is_empty(), "back to z = 0: order dropped");
}

/// A z change reorders the draw without any layout.
#[test]
fn z_change_costs_no_layout() {
    let mut ui = stack(&[0; 5]);
    ui.render(VIEW);
    let revs = ui.host.revs;
    let before = ui.counters();
    let mut t = Transaction::new(2);
    t.z(2, 3);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.host.revs.layout_input, revs.layout_input);
    assert!(ui.host.dirty.layout.is_empty());
    assert_eq!(painted(&mut ui), [0, 1, 3, 4, 5, 2]);
    let spent = ui.counters().since(&before);
    assert_eq!((spent.layout_passes, spent.layout_nodes), (0, 0));
    assert_eq!((spent.draw_orders, spent.chunks_built), (1, 0));
}

/// Tab and the accessibility tree keep tree order, whatever the z.
#[test]
fn tab_and_a11y_keep_tree_order() {
    let mut ui = stack(&[2, 1, 0]);
    let mut t = Transaction::new(2);
    for id in 1..=3 {
        t.interaction(id, mask::FOCUS, true);
    }
    ui.apply_txn(&t).unwrap();
    assert_eq!(painted(&mut ui), [0, 3, 2, 1]);
    let tab = |ui: &mut Ui| {
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Tab,
            ..KeyInput::default()
        }));
        ui.focused().map(|n| n.0)
    };
    let ring: Vec<_> = (0..3).map(|_| tab(&mut ui)).collect();
    assert_eq!(ring, [Some(1), Some(2), Some(3)]);
    let tree = ui.a11y_tree(VIEW);
    let parent = &tree
        .nodes
        .iter()
        .find(|(id, _)| *id == crate::a11y::aid(NodeId(0)))
        .unwrap()
        .1;
    let kids: Vec<_> = (1..=3).map(|id| crate::a11y::aid(NodeId(id))).collect();
    assert_eq!(parent.children(), kids);
}

/// The app 0 with a button 6, and three layers at the root: a dialog
/// (container 1, z 70) holding box 2, a menu (container 3, z 50) owned
/// by box 2 and holding box 4, and a toast (container 5, z 80). Layer
/// boxes sit apart, so each point hits one of them or the app.
fn layered(menu_owner: u32) -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let fill = boxed(0.0, 0.0, 400.0, 400.0);
    view(&mut t, NIL, 0, &fill);
    view(&mut t, 0, 6, &boxed(0.0, 0.0, 400.0, 400.0));
    t.interaction(6, mask::POINTER_DOWN, true);
    for (layer, z, owner, content, x) in [
        (1, 70, NIL, 2, 0.0),
        (3, 50, menu_owner, 4, 100.0),
        (5, 80, NIL, 7, 200.0),
    ] {
        t.create(layer, NodeKind::View)
            .layout(layer, &fill)
            .layer(layer, owner)
            .z(layer, z)
            .append(NIL, layer);
        view(&mut t, layer, content, &boxed(x, 0.0, 100.0, 100.0));
    }
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    ui
}

fn roots(ui: &Ui) -> Vec<u32> {
    ui.host
        .paint_order(NodeId::NIL)
        .iter()
        .map(|n| n.0)
        .collect()
}

/// Layers sort by z, then open order, and never below their owner;
/// their own boxes let hits through to what is below.
#[test]
fn layers_sort_above_their_owner_and_let_hits_through() {
    let mut ui = layered(NIL);
    // Unowned, the menu's z 50 puts it under the dialog.
    assert_eq!(roots(&ui), [0, 3, 1, 5]);
    let mut ui_owned = layered(2);
    assert_eq!(roots(&ui_owned), [0, 1, 3, 5]);
    assert_eq!(painted(&mut ui_owned), [0, 6, 2, 4, 7]);
    // Each layer's box is hit; everywhere else reaches the app button
    // through three full-window containers.
    assert_eq!(ui_owned.hit_test(50.0, 50.0), Some(NodeId(2)));
    assert_eq!(ui_owned.hit_test(150.0, 50.0), Some(NodeId(4)));
    assert_eq!(ui_owned.hit_test(250.0, 50.0), Some(NodeId(7)));
    assert_eq!(ui_owned.hit_test(50.0, 250.0), Some(NodeId(6)));
    ui_owned.dispatch(&Event::PointerDown {
        x: 350.0,
        y: 350.0,
        button: Button::Primary,
        mods: Mods::default(),
    });
    assert!(ui_owned.take_events().iter().any(|e| e.node == 6));

    // The owner moves into the toast: the menu follows it up.
    let mut t = Transaction::new(2);
    t.append(5, 2);
    ui_owned.apply_txn(&t).unwrap();
    assert_eq!(roots(&ui_owned), [0, 1, 5, 3]);
    // Dropping the owner puts the menu back at its z.
    let mut t = Transaction::new(3);
    t.layer(3, NIL);
    ui_owned.apply_txn(&t).unwrap();
    assert_eq!(roots(&ui_owned), [0, 3, 1, 5]);
    // A layer is a sibling rule, not a stacking context: the menu at
    // z 90 goes above the toast.
    let mut t = Transaction::new(2);
    t.z(3, 90);
    ui.apply_txn(&t).unwrap();
    assert_eq!(roots(&ui), [0, 1, 5, 3]);
}

/// Owners that close a cycle (each layer owned from inside the other)
/// still sort: the cycle is cut, and every child appears once.
#[test]
fn owner_cycles_still_sort() {
    let mut ui = layered(2);
    let mut t = Transaction::new(2);
    t.layer(1, 4);
    ui.apply_txn(&t).unwrap();
    let mut order = roots(&ui);
    order.sort();
    assert_eq!(order, [0, 1, 3, 5]);
}

/// A removed owner's layers become unowned: the node that reuses its
/// id does not adopt them.
#[test]
fn a_reused_owner_id_adopts_nothing() {
    let mut ui = layered(2);
    let mut t = Transaction::new(2);
    t.remove(2);
    view(&mut t, 5, 2, &boxed(0.0, 0.0, 10.0, 10.0));
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.host.owners[&3], NIL);
    assert_eq!(roots(&ui), [0, 3, 1, 5]);
}

/// The paint order the oracle expects of `parent`, where it can say:
/// with no layer raised by an owner sibling, the stable sort by z;
/// otherwise the same children, each raised layer after its owner's
/// sibling (outside ownership cycles, which are cut somewhere).
fn check_order(ui: &Ui, parent: NodeId, why: &str) {
    let host = &ui.host;
    let kids = host.children(parent);
    let order = host.paint_order(parent);
    let mut same: Vec<NodeId> = order.to_vec();
    same.sort();
    let mut tree = kids.to_vec();
    tree.sort();
    assert_eq!(same, tree, "{why}: a permutation of the children");
    // The sibling holding each child's owner, by index in `kids`.
    let holder: Vec<Option<usize>> = kids
        .iter()
        .map(|&c| {
            let mut cur = NodeId(*host.owners.get(&c.0)?);
            if cur.0 == NIL {
                return None;
            }
            while host.parent(cur) != parent {
                cur = host.parent(cur);
                if !cur.is_node() {
                    return None;
                }
            }
            (cur != c).then(|| kids.iter().position(|&k| k == cur).unwrap())
        })
        .collect();
    if holder.iter().all(Option::is_none) {
        let mut by_z = kids.to_vec();
        by_z.sort_by_key(|c| host.spatial[c.index()].z);
        assert_eq!(&order[..], &by_z[..], "{why}: stable sort by z");
        return;
    }
    let at = |c: NodeId| order.iter().position(|&o| o == c).unwrap();
    for (i, h) in holder.iter().enumerate() {
        let Some(mut h) = *h else { continue };
        let owner_sibling = kids[h];
        // Skip a layer on an ownership cycle.
        let mut cyclic = false;
        for _ in 0..kids.len() {
            if h == i {
                cyclic = true;
                break;
            }
            let Some(next) = holder[h] else { break };
            h = next;
        }
        if !cyclic {
            assert!(
                at(kids[i]) > at(owner_sibling),
                "{why}: layer {:?} sorts below its owner's sibling {owner_sibling:?}",
                kids[i]
            );
        }
    }
}

/// Every attached node, depth first in paint order: what the scene
/// draws when every box overlaps and nothing clips.
fn drawn(ui: &Ui, parent: NodeId, out: &mut Vec<u32>) {
    for &c in ui.host.paint_order(parent).iter() {
        out.push(c.0);
        drawn(ui, c, out);
    }
}

/// Random forests of overlapping views reshaped by z, layers with random
/// owners, moves and detaches, and removals whose ids are reused at
/// once, checked against an oracle that knows nothing of the sort keys:
/// before the refresh (a reader sorting on the spot) and after it, the
/// drawn order, and hits (front to back, passing layer boxes).
#[test]
fn paint_order_matches_an_oracle() {
    let full = boxed(0.0, 0.0, 100.0, 100.0);
    for seed in 1..=40u64 {
        let mut rng = Rng::new(seed * 0x51ED);
        let mut ui = Ui::new(1.0);
        let (mut next, mut free) = (0u32, Vec::new());
        for round in 0..60u64 {
            let live: Vec<u32> = (0..ui.host.slot_count() as u32)
                .filter(|&i| ui.host.is_live(NodeId(i)))
                .collect();
            let any = |rng: &mut Rng| live[rng.below(live.len() as u32) as usize];
            let mut t = Transaction::new(round + 1);
            match if live.is_empty() { 0 } else { rng.below(7) } {
                0 | 1 => {
                    let id = free.pop().unwrap_or_else(|| {
                        next += 1;
                        next - 1
                    });
                    let parent = if live.is_empty() || rng.chance(0.2) {
                        NIL
                    } else {
                        any(&mut rng)
                    };
                    view(&mut t, parent, id, &full);
                    if rng.chance(0.4) {
                        t.z(id, rng.below(5) as i32 - 2);
                    }
                }
                2 => {
                    // The slot goes straight to a new node.
                    let id = any(&mut rng);
                    t.remove(id);
                    let parent = live
                        .iter()
                        .copied()
                        .filter(|&p| p != id)
                        .nth(rng.below(live.len() as u32) as usize)
                        .unwrap_or(NIL);
                    view(&mut t, parent, id, &full);
                }
                3 => {
                    let (id, to) = (any(&mut rng), any(&mut rng));
                    if rng.chance(0.2) {
                        t.detach(id);
                    } else if rng.chance(0.2) {
                        t.append(NIL, id);
                    } else if !ui.ancestors(NodeId(to)).any(|n| n.0 == id) {
                        t.append(to, id);
                    }
                }
                4 | 5 => {
                    t.z(any(&mut rng), rng.below(5) as i32 - 2);
                }
                _ => {
                    let (id, owner) = (any(&mut rng), any(&mut rng));
                    let owner = if owner == id || rng.chance(0.2) {
                        NIL
                    } else {
                        owner
                    };
                    t.layer(id, owner);
                }
            }
            ui.apply_txn(&t).unwrap();
            let attached: Vec<NodeId> = std::iter::once(NodeId::NIL)
                .chain((0..ui.host.slot_count() as u32).map(NodeId).filter(|&n| {
                    ui.host.is_live(n)
                        && ui
                            .ancestors(n)
                            .last()
                            .is_some_and(|r| ui.host.parent(r) == NodeId::NIL)
                }))
                .collect();
            let why = format!("seed {seed} round {round}");
            let stale: Vec<Vec<NodeId>> = attached
                .iter()
                .map(|&p| {
                    check_order(&ui, p, &format!("{why}, before the refresh"));
                    ui.host.paint_order(p).to_vec()
                })
                .collect();
            let shown = painted(&mut ui);
            for (&p, before) in attached.iter().zip(&stale) {
                check_order(&ui, p, &why);
                assert_eq!(
                    &ui.host.paint_order(p)[..],
                    &before[..],
                    "{why}: refresh changed {p:?}"
                );
            }
            let mut expect = Vec::new();
            drawn(&ui, NodeId::NIL, &mut expect);
            assert_eq!(shown, expect, "{why}: drawn order");
            let front = expect.iter().rev().find(|&&n| !ui.host.is_layer(NodeId(n)));
            assert_eq!(
                ui.hit_test(50.0, 50.0).map(|n| n.0),
                front.copied(),
                "{why}: hit"
            );
            assert_eq!(
                ui.hit_test_walk(50.0, 50.0),
                ui.hit_test(50.0, 50.0),
                "{why}: walk"
            );
        }
    }
}

/// A sorted parent's id, reused at once, starts in tree order.
#[test]
fn a_recycled_sorted_parent_starts_in_tree_order() {
    let mut ui = stack(&[2, 1, 0]);
    assert_eq!(painted(&mut ui), [0, 3, 2, 1]);
    let mut t = Transaction::new(2);
    t.remove(0);
    view(&mut t, NIL, 0, &boxed(0.0, 0.0, 200.0, 200.0));
    for id in 1..=3 {
        t.append(0, id);
    }
    ui.apply_txn(&t).unwrap();
    // The children keep their z: sorted again, from their new tree order.
    assert_eq!(painted(&mut ui), [0, 3, 2, 1]);
    let mut t = Transaction::new(3);
    t.remove(0);
    view(&mut t, NIL, 0, &boxed(0.0, 0.0, 200.0, 200.0));
    view(&mut t, 0, 4, &boxed(0.0, 0.0, 100.0, 100.0));
    view(&mut t, 0, 5, &boxed(0.0, 0.0, 100.0, 100.0));
    ui.apply_txn(&t).unwrap();
    assert_eq!(painted(&mut ui), [0, 4, 5]);
    assert!(!ui.host.orders.contains_key(&0));
}

/// A z and a layer set while detached take effect on attaching.
#[test]
fn z_and_layer_set_while_detached_apply_on_attach() {
    let mut ui = stack(&[0, 0, 0]);
    painted(&mut ui);
    let mut t = Transaction::new(2);
    t.detach(1).detach(2);
    ui.apply_txn(&t).unwrap();
    assert_eq!(painted(&mut ui), [0, 3]);
    let mut t = Transaction::new(3);
    t.z(1, 5).layer(2, 3);
    ui.apply_txn(&t).unwrap();
    assert_eq!(painted(&mut ui), [0, 3]);
    let mut t = Transaction::new(4);
    t.place(0, 1, 3).place(0, 2, 1);
    ui.apply_txn(&t).unwrap();
    // Tree order 2, 1, 3: 1 rises by z, 2 above its owner 3.
    assert_eq!(painted(&mut ui), [0, 3, 2, 1]);
    assert_eq!(ui.hit_test(50.0, 50.0), Some(NodeId(1)));
}

/// A layer is a view: anything else is a structural error.
#[test]
fn layer_on_a_non_view_is_rejected() {
    let mut ui = stack(&[0]);
    let mut t = Transaction::new(2);
    t.create(9, NodeKind::Text).append(0, 9).layer(9, NIL);
    assert_eq!(
        ui.apply_txn(&t),
        Err(crate::wire::WireError::Invalid("layer on a non-view node"))
    );
    assert!(!ui.host.is_live(NodeId(9)));
}
