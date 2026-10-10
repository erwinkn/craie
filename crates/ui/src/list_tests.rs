//! The lists contract's ops (protocol 20, `docs/contracts/lists.md`):
//! patch validation, stale bases, measurements through patches, rows
//! tagged by item, and seeded patches against a clean rebuild.

use craie_core::rng::Rng;

use crate::events::out_kind;
use crate::geom::Size;
use crate::list::ItemIndex;
use crate::mutation::{Item, ListOp, Mutation, NIL, NodeKind, Template, Transaction};
use crate::ui::Ui;
use crate::wire::{self, WireError};

const LIST: u32 = 1;
const WIDTH: f32 = 400.0;

type BadOp = (&'static str, fn(&mut Transaction));

fn items(ids: impl IntoIterator<Item = u32>) -> Vec<Item> {
    ids.into_iter().map(|id| Item::sized(id, 0, 40.0)).collect()
}

/// One transaction through the wire.
fn send(ui: &mut Ui, f: impl FnOnce(&mut Transaction)) -> Result<(), WireError> {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply(&wire::encode(&t)).map(|_| ())
}

/// A list of items `1..=n` at revision 1, estimating at `WIDTH`.
fn list(n: u32) -> Ui {
    let mut ui = Ui::new(1.0);
    send(&mut ui, |t| {
        t.create(0, NodeKind::View).append(NIL, 0);
        t.create(LIST, NodeKind::List)
            .list_config2(LIST, 600.0, -1.0, 3.0, 48.0, 0, &[])
            .list_patch(LIST, 0, 1, &[ListOp::splice(0, 0, &items(1..=n))])
            .append(0, LIST);
    })
    .unwrap();
    ui.host.lists.estimate(&mut ui.text, LIST, WIDTH);
    ui
}

fn ids(ui: &Ui) -> Vec<u32> {
    let l = ui.host.lists.get(LIST).unwrap();
    l.items.iter().map(|d| d.id).collect()
}

fn revision(ui: &Ui) -> u32 {
    ui.host.lists.get(LIST).unwrap().revision
}

fn measure(ui: &mut Ui, i: usize, v: f32) {
    let l = ui.host.lists.map.get_mut(&LIST).unwrap();
    l.extents.measure(i, v);
}

/// Malformed descriptors and ops, ranges against the list as the batch
/// leaves it, identities, configs and row tags: any failure rejects the
/// whole transaction.
#[test]
fn list_patches_validate_atomically() {
    let mut ui = list(4);
    let bad: [BadOp; 17] = [
        ("patch on a view", |t| {
            t.list_patch(0, 0, 1, &[]);
        }),
        ("splice past the end", |t| {
            t.list_patch(LIST, 2, 3, &[ListOp::splice(5, 0, &[])]);
        }),
        ("remove past the end", |t| {
            t.list_patch(LIST, 2, 3, &[ListOp::splice(2, 3, &[])]);
        }),
        ("move past the end", |t| {
            let m = ListOp::Move {
                from: 3,
                count: 2,
                to: 0,
            };
            t.list_patch(LIST, 2, 3, &[m]);
        }),
        ("move to past the rest", |t| {
            let m = ListOp::Move {
                from: 0,
                count: 1,
                to: 4,
            };
            t.list_patch(LIST, 2, 3, &[m]);
        }),
        ("update past the end", |t| {
            t.list_patch(LIST, 2, 3, &[ListOp::update(3, &items([4, 5]))]);
        }),
        ("update of another identity", |t| {
            t.list_patch(LIST, 2, 3, &[ListOp::update(0, &items([2]))]);
        }),
        ("an identity twice in the list", |t| {
            t.list_patch(LIST, 2, 3, &[ListOp::splice(0, 0, &items([3]))]);
        }),
        ("an identity twice in a splice", |t| {
            t.list_patch(LIST, 2, 3, &[ListOp::splice(0, 0, &items([9, 9]))]);
        }),
        ("a NIL identity", |t| {
            t.list_patch(LIST, 2, 3, &[ListOp::splice(0, 0, &items([NIL]))]);
        }),
        ("failed and loaded", |t| {
            let mut d = Item::sized(9, 0, 40.0);
            d.flags |= Item::FAILED;
            t.list_patch(LIST, 2, 3, &[ListOp::splice(0, 0, &[d])]);
        }),
        ("a negative estimate", |t| {
            let d = Item::sized(9, 0, -1.0);
            t.list_patch(LIST, 2, 3, &[ListOp::splice(0, 0, &[d])]);
        }),
        ("unknown item flags", |t| {
            let mut d = Item::sized(9, 0, 40.0);
            d.flags |= 1 << 5;
            t.list_patch(LIST, 2, 3, &[ListOp::splice(0, 0, &[d])]);
        }),
        ("a negative retain", |t| {
            t.list_config2(LIST, 600.0, -1.0, -1.0, 48.0, 0, &[]);
        }),
        ("a negative band width", |t| {
            let bands = Template::Widths(vec![(600.0, 40.0), (-1.0, 60.0)]);
            t.list_config2(LIST, 600.0, -1.0, 3.0, 48.0, 0, &[bands]);
        }),
        ("a text template without a character width", |t| {
            let text = Template::Text {
                base: 0.0,
                inset: 0.0,
                font_size: 14.0,
                line_height: 20.0,
                char_width: 0.0,
            };
            t.list_config2(LIST, 600.0, -1.0, 3.0, 48.0, 0, &[text]);
        }),
        ("a row of a view", |t| {
            t.list_row(LIST, 0, 2, 1);
        }),
    ];
    for (what, f) in bad {
        let r = send(&mut ui, |t| {
            // A valid patch first (to revision 2): it must not apply.
            t.list_patch(LIST, 1, 2, &[ListOp::update(0, &items([1]))]);
            f(t);
        });
        assert!(r.is_err(), "{what}");
        assert_eq!((ids(&ui), revision(&ui)), (vec![1, 2, 3, 4], 1), "{what}");
    }
    // Malformed op streams reject in the executor (the direct API) and
    // the decoder.
    let mut ops = ListOp::pack(&[ListOp::update(0, &items([1]))]);
    for (what, bytes) in [
        ("trailing byte", [ops.as_slice(), &[0]].concat()),
        ("truncated", ops[..ops.len() - 1].to_vec()),
        ("unknown op", {
            ops[0] = 7;
            ops.clone()
        }),
    ] {
        let mut t = Transaction::new(ui.seq + 1);
        t.push(Mutation::ListPatch {
            id: LIST,
            base: 1,
            next: 2,
            ops: bytes.into(),
        });
        assert!(ui.apply_txn(&t).is_err(), "{what}");
        assert_eq!(revision(&ui), 1, "{what}");
    }
    // Valid: a move and an update in one patch, then a splice in a
    // second patch on the revision the first leaves.
    send(&mut ui, |t| {
        let m = ListOp::Move {
            from: 3,
            count: 1,
            to: 0,
        };
        t.list_patch(LIST, 1, 2, &[m, ListOp::update(0, &items([4]))])
            .list_patch(LIST, 2, 7, &[ListOp::splice(4, 0, &items([9]))]);
    })
    .unwrap();
    assert_eq!((ids(&ui), revision(&ui)), (vec![4, 1, 2, 3, 9], 7));
}

/// A patch with a stale base is skipped with a `LIST_RESYNC` (the
/// list's revision, the patch's base), not applied and not an error;
/// its ops aren't checked against a list they weren't made for. A later
/// patch in the batch checks against what applied.
#[test]
fn stale_patches_resync() {
    let mut ui = list(3);
    ui.take_events();
    send(&mut ui, |t| {
        t.list_patch(LIST, 0, 5, &[ListOp::splice(99, 0, &[])])
            .list_patch(LIST, 1, 2, &[ListOp::splice(0, 1, &[])])
            .list_patch(LIST, 5, 6, &[ListOp::splice(0, 2, &[])]);
    })
    .unwrap();
    assert_eq!((ids(&ui), revision(&ui)), (vec![2, 3], 2));
    let resyncs: Vec<_> = (ui.take_events().into_iter())
        .filter(|e| e.kind == out_kind::LIST_RESYNC)
        .map(|e| (e.node, e.revision, e.key))
        .collect();
    assert_eq!(resyncs, [(LIST, 1, 0), (LIST, 2, 5)]);
}

/// A measurement holds while the item keeps its identity and version:
/// through a move, and through a splice that removes and re-inserts it.
/// A new version takes its new estimate until measured; an unmeasured
/// item always takes its descriptor's estimate.
#[test]
fn measurements_follow_identity_and_version() {
    let mut ui = list(5);
    for i in 0..4 {
        measure(&mut ui, i, 100.0 + i as f32);
    }
    let v = |id, version, size| Item::sized(id, version, size);
    send(&mut ui, |t| {
        let m = ListOp::Move {
            from: 0,
            count: 1,
            to: 4,
        };
        t.list_patch(
            LIST,
            1,
            2,
            &[
                m,                                                     // 2 3 4 5 1
                ListOp::splice(0, 2, &[v(3, 0, 10.0), v(2, 1, 20.0)]), // 3 2' 4 5 1
                ListOp::update(2, &[v(4, 0, 30.0)]),                   // 4: same version
                ListOp::update(3, &[v(5, 0, 50.0)]),                   // 5: unmeasured
            ],
        );
    })
    .unwrap();
    assert_eq!(ids(&ui), [3, 2, 4, 5, 1]);
    assert_eq!(
        extents(&ui),
        [
            (102.0, true), // 3: re-inserted, same version
            (20.0, false), // 2: a new version
            (103.0, true),
            (50.0, false),
            (100.0, true), // 1: moved
        ]
    );
    // A new loaded state at the same version is new content too (the
    // kit's rule): unloading 3, failing 4 and re-inserting 1 unloaded
    // drop their measurements.
    let state = |id, flags| Item {
        flags: Item::NUMERIC | flags,
        ..v(id, 0, 60.0)
    };
    send(&mut ui, |t| {
        t.list_patch(
            LIST,
            2,
            3,
            &[
                ListOp::update(0, &[state(3, 0)]),
                ListOp::update(2, &[state(4, Item::FAILED)]),
                ListOp::splice(4, 1, &[state(1, 0)]),
            ],
        );
    })
    .unwrap();
    let measured: Vec<bool> = extents(&ui).iter().map(|e| e.1).collect();
    assert_eq!(measured, [false; 5]);
}

/// Row `id`'s height becomes `h`.
fn height(t: &mut Transaction, id: u32, h: f32) {
    let s = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::auto(),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    };
    let style = t.style(&s);
    t.push(Mutation::Layout { id, style });
}

/// A row with fixed height `h`, not placed yet.
fn row(t: &mut Transaction, id: u32, h: f32) {
    t.create(id, NodeKind::View);
    height(t, id, h);
}

/// A tagged row stands for its item wherever patches move it, hides
/// once the item is gone, and measures only at the version it was
/// rendered for: a patch to other items needs no new tag. `LIST_INDEX`
/// clears a tag.
#[test]
fn tagged_rows_follow_their_items() {
    let mut ui = list(4);
    send(&mut ui, |t| {
        // Tags before the rows are placed resolve all the same.
        row(t, 10, 77.0);
        row(t, 11, 33.0);
        t.list_row(10, LIST, 2, 0)
            .list_row(11, LIST, 3, 0)
            .append(LIST, 10)
            .append(LIST, 11);
    })
    .unwrap();
    let index = |ui: &Ui, row: usize| ui.host.list_index[row];
    assert_eq!((index(&ui, 10), index(&ui, 11)), (1, 2));
    ui.render(Size::new(WIDTH, 600.0));
    let measured = |ui: &Ui, i: usize| {
        let l = ui.host.lists.get(LIST).unwrap();
        l.extents.is_measured(i).then(|| l.extents.size(i))
    };
    assert_eq!(
        (measured(&ui, 1), measured(&ui, 2)),
        (Some(77.0), Some(33.0))
    );

    // Item 3 to the front and a new version of item 2: item 3's row
    // measures on, item 2's is placed but doesn't measure.
    send(&mut ui, |t| {
        let m = ListOp::Move {
            from: 2,
            count: 1,
            to: 0,
        };
        let new = Item::sized(2, 1, 40.0);
        t.list_patch(LIST, 1, 2, &[m, ListOp::update(2, &[new])]);
        height(t, 11, 44.0);
    })
    .unwrap();
    assert_eq!(ids(&ui), [3, 1, 2, 4]);
    assert_eq!((index(&ui, 10), index(&ui, 11)), (2, 0));
    ui.render(Size::new(WIDTH, 600.0));
    assert_eq!((measured(&ui, 2), measured(&ui, 0)), (None, Some(44.0)));
    send(&mut ui, |t| {
        t.list_row(10, LIST, 2, 1);
    })
    .unwrap();
    ui.render(Size::new(WIDTH, 600.0));
    assert_eq!(measured(&ui, 2), Some(77.0));

    // Item 2 removed: its row hides. LIST_INDEX takes row 11 off its tag.
    send(&mut ui, |t| {
        t.list_patch(LIST, 2, 3, &[ListOp::splice(2, 1, &[])])
            .list_index(11, 3);
    })
    .unwrap();
    assert_eq!((index(&ui, 10), index(&ui, 11)), (NIL, 3));
    assert!(!ui.host.lists.rows.contains_key(&11));
    send(&mut ui, |t| {
        t.list_patch(LIST, 3, 4, &[ListOp::splice(0, 1, &[])]);
    })
    .unwrap();
    assert_eq!(index(&ui, 11), 3, "an untagged row keeps its index");
}

/// The test's own copy of the rules: items, and each one's measurement.
type Model = Vec<(Item, Option<f32>)>;

/// A random op valid against `m`, applied to it. Fresh identities come
/// from `next`; versions change on about a third of the descriptors.
fn random_op(rng: &mut Rng, m: &mut Model, next: &mut u32) -> ListOp<'static> {
    let n = m.len() as u32;
    // A new descriptor: sometimes a new version, sometimes another
    // loaded state (loaded, unloaded, failed), always a new estimate.
    let redo = |rng: &mut Rng, d: Item| {
        let version = d.version + rng.chance(0.3) as u32;
        let mut e = Item::sized(d.id, version, 10.0 + rng.below(90) as f32);
        e.flags = Item::NUMERIC | (d.flags & !Item::NUMERIC);
        if rng.chance(0.2) {
            const STATES: [u8; 3] = [Item::LOADED, 0, Item::FAILED];
            e.flags = Item::NUMERIC | STATES[rng.below(3) as usize];
        }
        e
    };
    match rng.below(3) {
        0 => {
            let at = rng.below(n + 1);
            let remove = rng.below(n - at + 1).min(5);
            let removed: Vec<(Item, Option<f32>)> =
                m.drain(at as usize..(at + remove) as usize).collect();
            // Some removed items come back (a reorder within the splice).
            let mut new: Model = Vec::new();
            for &(d, measured) in &removed {
                if rng.chance(0.5) {
                    let d2 = redo(rng, d);
                    new.push((d2, measured.filter(|_| d2.same_content(&d))));
                }
            }
            for _ in 0..rng.below(4) {
                *next += 1;
                new.push((Item::sized(*next, 0, 10.0 + rng.below(90) as f32), None));
            }
            if new.len() > 1 {
                let k = rng.below(new.len() as u32) as usize;
                new.swap(0, k);
            }
            let descs: Vec<Item> = new.iter().map(|e| e.0).collect();
            m.splice(at as usize..at as usize, new);
            ListOp::splice(at, remove, &descs)
        }
        1 if n > 0 => {
            let from = rng.below(n);
            let count = 1 + rng.below((n - from).min(4));
            let to = rng.below(n - count + 1);
            let moved: Vec<_> = m.drain(from as usize..(from + count) as usize).collect();
            m.splice(to as usize..to as usize, moved);
            ListOp::Move { from, count, to }
        }
        _ if n > 0 => {
            let at = rng.below(n);
            let count = 1 + rng.below((n - at).min(4));
            let mut descs = Vec::new();
            for e in &mut m[at as usize..(at + count) as usize] {
                let d = redo(rng, e.0);
                e.1 = e.1.filter(|_| d.same_content(&e.0));
                e.0 = d;
                descs.push(d);
            }
            ListOp::update(at, &descs)
        }
        _ => ListOp::splice(0, 0, &[]),
    }
}

/// Every extent of the list: (size, measured).
fn extents(ui: &Ui) -> Vec<(f32, bool)> {
    let l = ui.host.lists.get(LIST).unwrap();
    (0..l.len() as usize)
        .map(|i| (l.extents.size(i), l.extents.is_measured(i)))
        .collect()
}

/// Seeded patches (splices that re-insert, moves, updates, several per
/// patch, measurements in between) leave the list equal to a clean
/// rebuild from its final descriptors and the measurements the rules
/// keep: items, identity index, extents. Rows tagged by item, some with
/// items still to come, stand for their items' indices throughout.
#[test]
fn seeded_patches_equal_a_clean_rebuild() {
    let mut rng = Rng::new(20);
    for round in 0..60 {
        let n = rng.below(40);
        let mut ui = list(n);
        let mut m: Model = items(1..=n).into_iter().map(|d| (d, None)).collect();
        let mut next = 1000;
        let tags: Vec<(u32, u32)> = (100..104)
            .map(|row| (row, 1 + rng.below(n + 2) + 999 * rng.chance(0.3) as u32))
            .collect();
        send(&mut ui, |t| {
            for &(row, item) in &tags {
                t.create(row, NodeKind::View).list_row(row, LIST, item, 0);
            }
        })
        .unwrap();
        for step in 0..12 {
            for (i, e) in m.iter_mut().enumerate() {
                if rng.chance(0.3) {
                    let v = 20.0 + rng.below(200) as f32;
                    e.1 = Some(v);
                    measure(&mut ui, i, v);
                }
            }
            let ops: Vec<ListOp> = (0..1 + rng.below(4))
                .map(|_| random_op(&mut rng, &mut m, &mut next))
                .collect();
            let base = revision(&ui);
            send(&mut ui, |t| {
                t.list_patch(LIST, base, base + 1, &ops);
            })
            .unwrap_or_else(|e| panic!("round {round} step {step}: {e:?}"));
            let expect: Vec<(f32, bool)> = m
                .iter()
                .map(|(d, v)| v.map_or((d.size().unwrap(), false), |v| (v, true)))
                .collect();
            assert_eq!(extents(&ui), expect, "round {round} step {step}");
            for &(row, item) in &tags {
                let at = m.iter().position(|e| e.0.id == item);
                let at = at.map_or(NIL, |i| i as u32);
                assert_eq!(
                    ui.host.list_index[row as usize], at,
                    "round {round} step {step}"
                );
            }
        }
        // The clean rebuild.
        let descs: Vec<Item> = m.iter().map(|e| e.0).collect();
        let mut clean = list(0);
        send(&mut clean, |t| {
            t.list_patch(LIST, 1, 2, &[ListOp::splice(0, 0, &descs)]);
        })
        .unwrap();
        let kept: Vec<(u32, f32)> = (m.iter().enumerate())
            .filter_map(|(i, e)| e.1.map(|v| (i as u32, v)))
            .collect();
        clean.host.lists.restore_measurements(LIST, WIDTH, &kept);
        clean.host.lists.estimate(&mut clean.text, LIST, WIDTH);
        assert_eq!(ids(&ui), ids(&clean), "round {round}");
        let (a, b) = (
            ui.host.lists.get(LIST).unwrap(),
            clean.host.lists.get(LIST).unwrap(),
        );
        assert_eq!(a.items, b.items, "round {round}");
        assert_eq!(
            a.index,
            ItemIndex::build(descs.iter().map(|d| d.id)),
            "round {round}"
        );
        assert_eq!(extents(&ui), extents(&clean), "round {round}");
    }
}

/// A random op against identities `m`, often invalid: a splice may
/// insert an identity present elsewhere (fine when it removes it), an
/// update may name another identity. Applies it to `m` and returns
/// whether it was valid; `m` is then unspecified if not.
fn tricky_op(rng: &mut Rng, m: &mut Vec<u32>, next: &mut u32) -> (ListOp<'static>, bool) {
    let n = m.len() as u32;
    let pick = |rng: &mut Rng, m: &[u32], next: &mut u32| {
        if !m.is_empty() && rng.chance(0.3) {
            m[rng.below(m.len() as u32) as usize]
        } else {
            *next += 1;
            *next
        }
    };
    match rng.below(3) {
        0 => {
            let at = rng.below(n + 1);
            let remove = rng.below(n - at + 1).min(5);
            let mut ids: Vec<u32> = (0..rng.below(4)).map(|_| pick(rng, m, next)).collect();
            ids.sort_unstable();
            ids.dedup();
            m.drain(at as usize..(at + remove) as usize);
            let ok = ids.iter().all(|i| !m.contains(i));
            m.splice(at as usize..at as usize, ids.iter().copied());
            (ListOp::splice(at, remove, &items(ids)), ok)
        }
        1 if n > 0 => {
            let from = rng.below(n);
            let count = 1 + rng.below((n - from).min(4));
            let to = rng.below(n - count + 1);
            let moved: Vec<u32> = m.drain(from as usize..(from + count) as usize).collect();
            m.splice(to as usize..to as usize, moved);
            (ListOp::Move { from, count, to }, true)
        }
        _ if n > 0 => {
            let at = rng.below(n);
            let count = 1 + rng.below((n - at).min(4));
            let mut ids = m[at as usize..(at + count) as usize].to_vec();
            if rng.chance(0.2) {
                let k = rng.below(count) as usize;
                ids[k] = pick(rng, m, next);
            }
            let ok = ids == m[at as usize..(at + count) as usize];
            (ListOp::update(at, &items(ids)), ok)
        }
        _ => (ListOp::splice(0, 0, &[]), true),
    }
}

/// Validation reads a batch's later edits through its earlier ones
/// (and copies the sequence past `MAX_EDITS`): seeded batches of tricky
/// ops, in one patch or several, are accepted exactly when a plain copy
/// of the identities says they are valid, and then apply as it does.
#[test]
fn seeded_batches_validate_as_a_copy() {
    let mut rng = Rng::new(38);
    let (mut accepted, mut rejected) = (0, 0);
    for round in 0..400 {
        let n = rng.below(30);
        let mut ui = list(n);
        let mut m: Vec<u32> = (1..=n).collect();
        let mut next = 1000;
        let count = if rng.chance(0.1) {
            40
        } else {
            1 + rng.below(6)
        };
        let mut ok = true;
        let ops: Vec<ListOp> = (0..count)
            .map(|_| {
                let (op, valid) = tricky_op(&mut rng, &mut m, &mut next);
                ok &= valid;
                op
            })
            .collect();
        let patches = 1 + rng.below(3) as usize;
        let r = send(&mut ui, |t| {
            for (k, chunk) in ops.chunks(ops.len().div_ceil(patches)).enumerate() {
                t.list_patch(LIST, 1 + k as u32, 2 + k as u32, chunk);
            }
        });
        assert_eq!(r.is_ok(), ok, "round {round}: {r:?}");
        if ok {
            assert_eq!(ids(&ui), m, "round {round}");
            accepted += 1;
        } else {
            assert_eq!(ids(&ui), (1..=n).collect::<Vec<_>>(), "round {round}");
            rejected += 1;
        }
    }
    assert!(accepted > 100 && rejected > 100, "{accepted} / {rejected}");
}
