//! The lists contract's ops (protocol 20, `docs/contracts/lists.md`):
//! patch validation, stale bases, measurements through patches, rows
//! tagged by item, and seeded patches against a clean rebuild.

use craie_core::rng::Rng;

use crate::events::out_kind;
use crate::geom::Size;
use crate::list::IdIndex;
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
        ("width bands out of order", |t| {
            let bands = Template::Widths(vec![(600.0, 40.0), (0.0, 60.0)]);
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
    let l = ui.host.lists.get(LIST).unwrap();
    let ext: Vec<(f32, bool)> = (0..5)
        .map(|i| (l.extents.size(i), l.extents.is_measured(i)))
        .collect();
    assert_eq!(
        ext,
        [
            (102.0, true), // 3: re-inserted, same version
            (20.0, false), // 2: a new version
            (103.0, true),
            (50.0, false),
            (100.0, true), // 1: moved
        ]
    );
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
    let redo = |rng: &mut Rng, d: Item| {
        let version = d.version + rng.chance(0.3) as u32;
        Item::sized(d.id, version, 10.0 + rng.below(90) as f32)
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
                    new.push((d2, measured.filter(|_| d2.version == d.version)));
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
                e.1 = e.1.filter(|_| d.version == e.0.version);
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
/// keep: items, identity index, extents.
#[test]
fn seeded_patches_equal_a_clean_rebuild() {
    let mut rng = Rng::new(20);
    for round in 0..60 {
        let n = rng.below(40);
        let mut ui = list(n);
        let mut m: Model = items(1..=n).into_iter().map(|d| (d, None)).collect();
        let mut next = 1000;
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
            a.ids,
            IdIndex::build(descs.iter().map(|d| d.id)),
            "round {round}"
        );
        assert_eq!(extents(&ui), extents(&clean), "round {round}");
    }
}

/// A window-sized scroller (400 x 200) holding a list of `n` items
/// 40 points tall (identities `1..=n`), under `policy`, laid out.
fn scrolled(n: u32, policy: crate::mutation::ListPolicy) -> Ui {
    let mut ui = Ui::new(1.0);
    let scroller = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        },
        overflow: taffy::Point {
            x: taffy::Overflow::Visible,
            y: taffy::Overflow::Scroll,
        },
        ..crate::host::default_style().to_taffy()
    };
    send(&mut ui, |t| {
        t.create(0, NodeKind::View)
            .layout(0, &scroller)
            .list_policy(0, policy)
            .append(NIL, 0);
        t.create(LIST, NodeKind::List)
            .list_config2(LIST, 0.0, -1.0, 3.0, 40.0, 0, &[])
            .list_patch(LIST, 0, 1, &[ListOp::splice(0, 0, &items(1..=n))])
            .append(0, LIST);
    })
    .unwrap();
    ui.render(VIEW);
    ui
}

const VIEW: Size = Size {
    width: 400.0,
    height: 200.0,
};

fn scroll(ui: &Ui) -> f32 {
    ui.scroll_offset(crate::host::NodeId(0))[1]
}

/// Policies and commands reject out-of-range numbers and unknown
/// bytes, in the executor and the decoder.
#[test]
fn list_policies_and_commands_validate() {
    use crate::mutation::{Align, Jump, ListPolicy};
    let mut ui = list(4);
    let bad: [BadOp; 5] = [
        ("a negative end threshold", |t| {
            let p = ListPolicy {
                end_threshold: -1.0,
                ..ListPolicy::default()
            };
            t.list_policy(0, p);
        }),
        ("a NaN inset", |t| {
            let p = ListPolicy {
                start_inset: f32::NAN,
                ..ListPolicy::default()
            };
            t.list_policy(0, p);
        }),
        ("a policy on an absent node", |t| {
            t.list_policy(77, ListPolicy::default());
        }),
        ("a command on a view", |t| {
            t.list_command(0, 1, 1, Jump::End);
        }),
        ("an infinite offset", |t| {
            t.list_command(LIST, 1, 1, Jump::Offset(f64::INFINITY));
        }),
    ];
    for (what, f) in bad {
        assert!(send(&mut ui, f).is_err(), "{what}");
    }
    // Unknown kind, alignment and anchor policy bytes fail to decode.
    let mut t = Transaction::new(ui.seq + 1);
    t.list_command(LIST, 1, 9, Jump::Index(2, Align::Center));
    let buf = wire::encode(&t);
    let at = (buf.windows(5))
        .position(|w| w[0] == wire::op::LIST_COMMAND && w[1..5] == LIST.to_le_bytes())
        .unwrap();
    let (kind, align) = (at + 13, at + 18);
    for (byte, value) in [(kind, 9u8), (align, 3)] {
        let mut b = buf.clone();
        b[byte] = value;
        assert!(wire::decode(&b).is_err(), "byte {byte} = {value}");
    }
    let mut t = Transaction::new(ui.seq + 1);
    t.list_policy(0, ListPolicy::default());
    let mut b = wire::encode(&t);
    let at = (b.windows(5))
        .position(|w| w[0] == wire::op::LIST_POLICY && w[1..5] == 0u32.to_le_bytes())
        .unwrap();
    b[at + 6] = 2;
    assert!(wire::decode(&b).is_err(), "anchor policy 2");
}

/// An index jump made for another item order is skipped; a key jump
/// isn't (identities don't move with the order). Jumps hold their item
/// at its alignment until the reader scrolls.
#[test]
fn jumps_hold_their_item_and_skip_stale_indices() {
    use crate::mutation::{Align, Jump, ListPolicy};
    let mut ui = scrolled(100, ListPolicy::default());
    send(&mut ui, |t| {
        t.list_command(LIST, 1, 1, Jump::Index(50, Align::Start));
    })
    .unwrap();
    ui.render(VIEW);
    assert_eq!(scroll(&ui), 2000.0);
    // Revision 2, then an index jump at revision 1: skipped.
    send(&mut ui, |t| {
        t.list_patch(LIST, 1, 2, &[ListOp::update(0, &[Item::sized(1, 1, 40.0)])])
            .list_command(LIST, 1, 2, Jump::Index(10, Align::Start));
    })
    .unwrap();
    ui.render(VIEW);
    assert_eq!(scroll(&ui), 2000.0, "a stale index");
    // A key jump at the stale revision, centered: item 11 is index 10.
    send(&mut ui, |t| {
        t.list_command(LIST, 1, 3, Jump::Item(11, Align::Center));
    })
    .unwrap();
    ui.render(VIEW);
    assert_eq!(scroll(&ui), 400.0 - 80.0);
    // Ten rows above it grow: it stays centered.
    send(&mut ui, |t| {
        let grown: Vec<Item> = (1..=10).map(|id| Item::sized(id, 2, 80.0)).collect();
        t.list_patch(LIST, 2, 3, &[ListOp::update(0, &grown)]);
    })
    .unwrap();
    ui.render(VIEW);
    assert_eq!(scroll(&ui), 800.0 - 80.0);
    let v = ui.list_viewport(crate::host::NodeId(LIST)).unwrap();
    assert_eq!(v.anchor, Some((11, 10, 80.0)));
    // A missing key does nothing.
    send(&mut ui, |t| {
        t.list_command(LIST, 3, 4, Jump::Item(999, Align::Start));
    })
    .unwrap();
    ui.render(VIEW);
    assert_eq!(scroll(&ui), 720.0);
}

/// A covered band at the top (`startInset`) isn't viewport: visible
/// rows, the offset and jump alignment start below it.
#[test]
fn a_covered_band_moves_the_viewport() {
    use crate::mutation::{Align, Jump, ListPolicy};
    let policy = ListPolicy {
        start_inset: 50.0,
        ..ListPolicy::default()
    };
    let mut ui = scrolled(100, policy);
    let v = ui.list_viewport(crate::host::NodeId(LIST)).unwrap();
    assert_eq!((v.offset, v.visible.clone()), (50.0, 1..5));
    send(&mut ui, |t| {
        t.list_command(LIST, 1, 1, Jump::Index(10, Align::Start));
    })
    .unwrap();
    ui.render(VIEW);
    assert_eq!(scroll(&ui), 350.0, "item 10's top under the band");
    let v = ui.list_viewport(crate::host::NodeId(LIST)).unwrap();
    assert_eq!((v.offset, v.anchor), (400.0, Some((11, 10, 0.0))));
}

/// A row stays when it lies on any longest increasing run of new
/// indices: a swap or a reversal moves nothing; a block moved far moves
/// alone.
#[test]
fn rows_on_any_longest_run_stay() {
    use crate::list::on_any_lis;
    assert_eq!(on_any_lis(&[1, 0]), [true, true]);
    assert_eq!(on_any_lis(&[3, 2, 1, 0]), [true; 4]);
    assert_eq!(on_any_lis(&[0, 2, 1, 3]), [true; 4]);
    assert_eq!(on_any_lis(&[2, 0, 1]), [false, true, true]);
    // K13: m25 and m26 moved to the top.
    let k13: Vec<u32> = [25, 26].into_iter().chain(0..25).chain(27..100).collect();
    let on = on_any_lis(&k13);
    assert!(!on[0] && !on[1] && on[2..].iter().all(|&b| b));
    assert!(on_any_lis(&[]).is_empty());
}

/// A batch's span, from its ops alone, bounds what it touched: the
/// first `lo` items and the last `suffix` keep their identities and
/// places, over seeded batches.
#[test]
fn batch_spans_bound_what_changed() {
    use crate::list::Span;
    let mut rng = Rng::new(7);
    for _ in 0..400 {
        let n = rng.below(20);
        let mut model: Model = (1..=n).map(|id| (Item::sized(id, 0, 40.0), None)).collect();
        let old: Vec<u32> = model.iter().map(|e| e.0.id).collect();
        let mut next = 1000;
        let ops: Vec<ListOp> = (0..1 + rng.below(3))
            .map(|_| random_op(&mut rng, &mut model, &mut next))
            .collect();
        let new: Vec<(u32, u32)> = model.iter().map(|e| (e.0.id, e.0.version)).collect();
        let Some((span, len)) = Span::of(n, &ops) else {
            continue;
        };
        assert_eq!(len as usize, new.len());
        assert!(span.lo + span.suffix <= n.min(len), "{span:?} {n} {len}");
        for i in 0..span.lo as usize {
            assert_eq!(
                (new[i].0, new[i].1),
                (old[i], 0),
                "prefix {i}: {span:?} {ops:?}"
            );
        }
        for k in 1..=span.suffix as usize {
            let (a, b) = (old[old.len() - k], new[new.len() - k]);
            assert_eq!((b.0, b.1), (a, 0), "suffix {k}: {span:?} {ops:?}");
        }
    }
}

/// A stick-to-end scroller holding no list has no anchor: its offset is
/// the reader's, through layouts that change its content.
#[test]
fn a_scroller_without_a_list_keeps_the_readers_offset() {
    use crate::mutation::{Anchor, ListPolicy};
    let mut ui = Ui::new(1.0);
    let scroller = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        },
        overflow: taffy::Point {
            x: taffy::Overflow::Visible,
            y: taffy::Overflow::Scroll,
        },
        ..crate::host::default_style().to_taffy()
    };
    let tall = |h: f32| taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::auto(),
            height: taffy::Dimension::length(h),
        },
        flex_shrink: 0.0,
        ..crate::host::default_style().to_taffy()
    };
    let policy = ListPolicy {
        mode: Anchor::StickToEnd,
        ..ListPolicy::default()
    };
    send(&mut ui, |t| {
        t.create(0, NodeKind::View)
            .layout(0, &scroller)
            .list_policy(0, policy)
            .append(NIL, 0);
        t.create(1, NodeKind::View)
            .layout(1, &tall(1000.0))
            .append(0, 1);
    })
    .unwrap();
    ui.render(VIEW);
    ui.scroll_to(crate::host::NodeId(0), 0.0, 300.0);
    ui.render(VIEW);
    send(&mut ui, |t| {
        t.layout(1, &tall(1200.0));
    })
    .unwrap();
    ui.render(VIEW);
    assert_eq!(scroll(&ui), 300.0);
}
