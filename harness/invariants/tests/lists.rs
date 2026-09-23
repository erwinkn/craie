//! Virtualized lists (ARCHITECTURE.md §7): only the reported range is
//! rendered and laid out, rows sit at their item offsets, a virtualized
//! list equals a plain column of every row once measured, and scroll
//! anchoring holds the top visible item (or the end) in place.

use craie_core::geom::Size;
use craie_harness::ListDriver;
use craie_ui::host::NodeId;
use craie_ui::mutation::{Anchor, ItemDesc, NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;

const VIEW: Size = Size {
    width: 320.0,
    height: 400.0,
};
const SCROLLER: u32 = 0;
const LIST: u32 = 1;
const FONT: f32 = 14.0;

/// Item `i`'s text: lengths vary so rows wrap to 1..4 lines.
fn text(i: u32) -> String {
    let words = 3 + (i * 7919) % 40;
    (0..words)
        .map(|w| ["alpha", "beta", "gamma", "delta", "eps"][((i + w) % 5) as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

/// Item `i`'s description; its identity is `i` unless given.
fn desc(i: u32) -> ItemDesc {
    desc_as(i, i)
}

fn desc_as(i: u32, id: u32) -> ItemDesc {
    ItemDesc {
        template: 0,
        text_len: text(i).chars().count() as u32,
        id,
        unchanged: false,
    }
}

fn scroller_style() -> taffy::Style {
    taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        },
        overflow: taffy::Point {
            x: taffy::Overflow::Visible,
            y: taffy::Overflow::Scroll,
        },
        ..craie_ui::host::default_style()
    }
}

/// A window-sized scroller holding a list of `n` items.
fn mount(n: u32, overscan: f32) -> (Ui, ListDriver) {
    let mut ui = Ui::new(2.0);
    let d = ListDriver::new(LIST, 100, FONT);
    let mut t = Transaction::new(1);
    t.create(SCROLLER, NodeKind::View)
        .layout(SCROLLER, &scroller_style())
        .append(NIL, SCROLLER);
    t.create(LIST, NodeKind::List)
        .list_config(LIST, overscan, 20.0, &[d.template()])
        .list_splice(LIST, 0, 0, &(0..n).map(desc).collect::<Vec<_>>())
        .append(SCROLLER, LIST);
    ui.apply_txn(&t).unwrap();
    (ui, d)
}

fn scroll_y(ui: &Ui) -> f32 {
    ui.scroll_offset(NodeId(SCROLLER))[1]
}

/// Screen y (logical, window) of item `i`'s row.
fn row_y(ui: &Ui, d: &ListDriver, i: u32) -> f32 {
    let id = d.rows[&i];
    ui.layouts.data(NodeId(LIST)).rect.origin.y + ui.layouts.data(NodeId(id)).rect.origin.y
        - scroll_y(ui)
}

#[test]
fn mount_renders_the_visible_range_only() {
    let (mut ui, mut d) = mount(10_000, 200.0);
    let commits = d.settle(&mut ui, VIEW, &text, 8);
    assert!(commits <= 3, "range converges ({commits} commits)");
    // Visible 400 pt plus 200 pt overscan below: a few dozen rows.
    let rendered = d.rows.len();
    assert!(
        (10..80).contains(&rendered),
        "rendered {rendered} of 10000 rows"
    );
    let l = ui.host.lists.get(LIST).unwrap();
    // The list is as tall as all its items; rows sit at their offsets.
    let list_h = ui.layouts.data(NodeId(LIST)).rect.size.height;
    assert!((list_h - l.total()).abs() < 1e-2, "{list_h}");
    for (&i, &id) in &d.rows {
        let y = ui.layouts.data(NodeId(id)).rect.origin.y;
        assert!((y - l.offset(i as usize)).abs() < 1e-3, "row {i}");
        assert!(l.extents.is_measured(i as usize));
        assert_eq!(
            ui.layouts.data(NodeId(id)).rect.size.height,
            l.extents.size(i as usize)
        );
    }
    // The scroller can reach the end of all items.
    let extent = ui.layouts.data(NodeId(SCROLLER)).scroll_extent[1];
    assert!((extent - (l.total() - VIEW.height)).abs() < 1e-2);
}

/// Scrolls to `y` and lets the list render its new range.
fn scroll(ui: &mut Ui, d: &mut ListDriver, y: f32) {
    ui.scroll_to(NodeId(SCROLLER), 0.0, y);
    d.settle(ui, VIEW, &text, 8);
}

/// The oracle: once every item has rendered (and been measured), each
/// row sits where it sits in a plain column holding every row with the
/// same box style (padding, border, row gap), the list is as tall, and
/// a sibling after it lands in the same place.
fn oracle(list_style: taffy::Style, what: &str) {
    let n = 200;
    let sibling = 2;
    let tail = |t: &mut Transaction| {
        t.create(sibling, NodeKind::View)
            .layout(
                sibling,
                &taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(50.0),
                        height: taffy::Dimension::length(30.0),
                    },
                    ..craie_ui::host::default_style()
                },
            )
            .append(SCROLLER, sibling);
    };
    let mut ui = Ui::new(2.0);
    let mut d = ListDriver::new(LIST, 100, FONT);
    let mut t = Transaction::new(1);
    t.create(SCROLLER, NodeKind::View)
        .layout(SCROLLER, &scroller_style())
        .append(NIL, SCROLLER);
    t.create(LIST, NodeKind::List)
        .layout(LIST, &list_style)
        .list_config(LIST, 100.0, 20.0, &[d.template()])
        .list_splice(LIST, 0, 0, &(0..n).map(desc).collect::<Vec<_>>())
        .append(SCROLLER, LIST);
    tail(&mut t);
    ui.apply_txn(&t).unwrap();
    d.settle(&mut ui, VIEW, &text, 8);
    // Scroll through the whole list: every row renders once.
    let mut y = 0.0;
    while y < ui.layouts.data(NodeId(SCROLLER)).scroll_extent[1] + VIEW.height {
        scroll(&mut ui, &mut d, y);
        y += VIEW.height * 0.5;
    }
    let l = ui.host.lists.get(LIST).unwrap();
    assert_eq!(
        l.extents.measured_count(),
        n as usize,
        "{what}: all measured"
    );

    let mut plain = Ui::new(2.0);
    let mut t = Transaction::new(1);
    t.create(SCROLLER, NodeKind::View)
        .layout(SCROLLER, &scroller_style())
        .append(NIL, SCROLLER);
    t.create(LIST, NodeKind::View)
        .layout(LIST, &list_style)
        .append(SCROLLER, LIST);
    for i in 0..n {
        t.create(100 + i, NodeKind::Text)
            .text(100 + i, text(i), FONT, 0xFFFF_FFFF)
            .append(LIST, 100 + i);
    }
    tail(&mut t);
    plain.apply_txn(&t).unwrap();
    plain.render(VIEW);
    let content_top = ui.layouts.data(NodeId(LIST)).content[1];
    for i in 0..n {
        let want = plain.layouts.data(NodeId(100 + i)).rect.origin.y;
        let got = content_top + l.offset(i as usize);
        assert!(
            (got - want).abs() < 1e-2,
            "{what}: item {i}: {got} vs {want}"
        );
    }
    let a = ui.layouts.data(NodeId(LIST)).rect;
    let b = plain.layouts.data(NodeId(LIST)).rect;
    assert!(
        (a.size.height - b.size.height).abs() < 1e-2,
        "{what}: height {a:?} vs {b:?}"
    );
    let a = ui.layouts.data(NodeId(sibling)).rect.origin;
    let b = plain.layouts.data(NodeId(sibling)).rect.origin;
    assert!((a.y - b.y).abs() < 1e-2, "{what}: sibling {a:?} vs {b:?}");
    // Rendered rows are laid out exactly like their plain twins.
    for (&i, &id) in &d.rows {
        let a = ui.layouts.data(NodeId(id)).rect;
        let b = plain.layouts.data(NodeId(100 + i)).rect;
        assert!(
            (a.origin.y - b.origin.y).abs() < 1e-2
                && (a.origin.x - b.origin.x).abs() < 1e-2
                && a.size == b.size,
            "{what}: row {i}: {a:?} vs {b:?}"
        );
    }
}

#[test]
fn virtualized_equals_plain_column() {
    use taffy::{LengthPercentage as LP, Rect};
    let base = craie_ui::host::default_style();
    oracle(base.clone(), "plain");
    let padded = taffy::Style {
        padding: Rect {
            left: LP::length(20.0),
            right: LP::length(20.0),
            top: LP::length(12.0),
            bottom: LP::length(8.0),
        },
        border: Rect {
            left: LP::length(3.0),
            right: LP::length(3.0),
            top: LP::length(1.0),
            bottom: LP::length(1.0),
        },
        ..base.clone()
    };
    oracle(padded.clone(), "padded");
    let gapped = taffy::Style {
        gap: taffy::Size {
            width: LP::length(0.0),
            height: LP::length(10.0),
        },
        ..base.clone()
    };
    oracle(gapped.clone(), "gap");
    oracle(
        taffy::Style {
            gap: gapped.gap,
            max_size: taffy::Size {
                width: taffy::LengthPercentageAuto::length(260.0),
                height: taffy::LengthPercentageAuto::auto(),
            },
            ..padded
        },
        "padded, gap, max width",
    );
    // Percentage row gaps: against a definite height (with padding), and
    // with an auto height (sized without the gap, then placed with it).
    oracle(
        taffy::Style {
            gap: taffy::Size {
                width: LP::length(0.0),
                height: LP::percent(0.1),
            },
            // Inner height 200 (padding 20, border 2): the gap is exactly
            // 20, so f32 sums in the plain column add no rounding.
            size: taffy::Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::length(222.0),
            },
            ..padded.clone()
        },
        "percent gap, definite height",
    );
    oracle(
        taffy::Style {
            gap: taffy::Size {
                width: LP::length(0.0),
                height: LP::percent(0.01),
            },
            ..base.clone()
        },
        "percent gap, auto height",
    );
}

/// Scrolling within the rendered range re-renders nothing and lays out
/// nothing; crossing it reports a new range once.
#[test]
fn scrolling_reports_ranges_with_hysteresis() {
    let (mut ui, mut d) = mount(5_000, 300.0);
    d.settle(&mut ui, VIEW, &text, 8);
    let first = d.rows.keys().next().copied();
    // 50 pt: well inside the overscan.
    let before = ui.counters();
    ui.scroll_to(NodeId(SCROLLER), 0.0, 50.0);
    ui.render(VIEW);
    let c = ui.counters().since(&before);
    assert!(!d.pump(&mut ui, &text), "no new range inside the overscan");
    assert_eq!((c.layout_passes, c.shapes), (0, 0), "{c:?}");
    // Far away: one new range, rows around the new viewport only.
    ui.scroll_to(NodeId(SCROLLER), 0.0, 20_000.0);
    ui.render(VIEW);
    assert!(d.pump(&mut ui, &text));
    d.settle(&mut ui, VIEW, &text, 8);
    assert_ne!(d.rows.keys().next().copied(), first);
    let l = ui.host.lists.get(LIST).unwrap();
    let top = l.item_at(scroll_y(&ui)) as u32;
    assert!(d.rows.contains_key(&top), "the top visible item renders");
    assert!(d.rows.len() < 80);
}

/// Screen y of item `i` from the list's extents (rendered or not).
fn item_y(ui: &Ui, i: u32) -> f32 {
    let l = ui.host.lists.get(LIST).unwrap();
    ui.layouts.data(NodeId(LIST)).rect.origin.y + l.offset(i as usize) - scroll_y(ui)
}

/// Keep-visible: when rows above the viewport measure differently from
/// their estimates, or items are inserted above, the top visible item
/// keeps its screen position. Without anchoring the same steps move it
/// (the negative control).
#[test]
fn keep_visible_holds_the_top_item() {
    for anchor in [Anchor::KeepVisible, Anchor::None] {
        let (mut ui, mut d) = mount(2_000, 100.0);
        // Deliberately poor estimates: a larger font than the rows use.
        let mut t = Transaction::new(2);
        let mut tpl = d.template();
        tpl.font_size = 24.0;
        t.list_config(LIST, 100.0, 20.0, &[tpl])
            .scroll_anchor(SCROLLER, anchor);
        ui.apply_txn(&t).unwrap();
        d.settle(&mut ui, VIEW, &text, 8);
        // Jump into the middle, then scroll up screen by screen: rows
        // that render above the top item measure smaller than their
        // estimates.
        scroll(&mut ui, &mut d, 30_000.0);
        let mut drift = 0.0f32;
        for _ in 0..4 {
            let y = scroll_y(&ui) - VIEW.height;
            ui.scroll_to(NodeId(SCROLLER), 0.0, y);
            ui.render(VIEW);
            let top = ui
                .host
                .lists
                .get(LIST)
                .unwrap()
                .extents
                .index_at(scroll_y(&ui)) as u32;
            let before = item_y(&ui, top);
            d.settle(&mut ui, VIEW, &text, 8);
            drift = drift.max((item_y(&ui, top) - before).abs());
        }
        match anchor {
            Anchor::KeepVisible => assert!(drift < 0.01, "measuring above: drift {drift}"),
            _ => assert!(drift > 10.0, "control: no anchor drifts ({drift})"),
        }
        if anchor != Anchor::KeepVisible {
            continue;
        }
        // Items inserted above the viewport: the same item stays put.
        let top = ui
            .host
            .lists
            .get(LIST)
            .unwrap()
            .extents
            .index_at(scroll_y(&ui)) as u32;
        let before = row_y(&ui, &d, top);
        let mut t = Transaction::new(3000);
        let added: Vec<ItemDesc> = (0..50).map(|i| desc_as(i + 7, 1_000_000 + i)).collect();
        t.list_splice(LIST, 10, 0, &added);
        d.spliced(&mut t, 10, 0, 50);
        ui.apply_txn(&t).unwrap();
        d.settle(&mut ui, VIEW, &text, 8);
        let after = row_y(&ui, &d, top + 50);
        assert!(
            (after - before).abs() < 0.01,
            "insert above: {before} -> {after}"
        );
    }
}

/// Stick-to-end: a scroller at its end stays there when items append;
/// one scrolled away from the end keeps its top item instead.
#[test]
fn stick_to_end_follows_appends() {
    let (mut ui, mut d) = mount(200, 100.0);
    let mut t = Transaction::new(2);
    t.scroll_anchor(SCROLLER, Anchor::StickToEnd);
    ui.apply_txn(&t).unwrap();
    d.settle(&mut ui, VIEW, &text, 8);
    let end = ui.layouts.data(NodeId(SCROLLER)).scroll_extent[1];
    scroll(&mut ui, &mut d, end);
    let mut n = 200;
    for k in 0..5 {
        let mut t = Transaction::new(10 + k);
        t.list_splice(LIST, n, 0, &[desc(n), desc(n + 1)]);
        n += 2;
        ui.apply_txn(&t).unwrap();
        d.settle(&mut ui, VIEW, &text, 8);
        let extent = ui.layouts.data(NodeId(SCROLLER)).scroll_extent[1];
        assert!(extent > end);
        assert_eq!(scroll_y(&ui), extent, "append {k}: at the end");
        assert!(d.rows.contains_key(&(n - 1)), "the new last item renders");
    }
    // Away from the end: appends do not move the view.
    let y = scroll_y(&ui) - 500.0;
    scroll(&mut ui, &mut d, y);
    let top = ui
        .host
        .lists
        .get(LIST)
        .unwrap()
        .extents
        .index_at(scroll_y(&ui)) as u32;
    let before = row_y(&ui, &d, top);
    let mut t = Transaction::new(99);
    t.list_splice(LIST, n, 0, &[desc(n)]);
    ui.apply_txn(&t).unwrap();
    d.settle(&mut ui, VIEW, &text, 8);
    assert_eq!(row_y(&ui, &d, top), before);
}

/// Layout visits only rendered rows: mounting a 100k-item list costs
/// about as many layout-node visits as the rows it renders.
#[test]
fn layout_cost_follows_rendered_rows() {
    let (mut ui, mut d) = mount(100_000, 200.0);
    let before = ui.counters();
    d.settle(&mut ui, VIEW, &text, 8);
    let c = ui.counters().since(&before);
    let rows = d.rows.len() as u64;
    assert!(
        c.layout_nodes < 20 * rows + 50,
        "{} layout visits for {rows} rows",
        c.layout_nodes
    );
    // Shapes: one per row plus the estimate metrics.
    assert!(
        c.shapes <= rows * 3 + 4,
        "{} shapes for {rows} rows",
        c.shapes
    );
}

/// A focused row stays rendered while it scrolls out of the range.
#[test]
fn focused_row_stays_rendered() {
    let (mut ui, mut d) = mount(5_000, 100.0);
    d.settle(&mut ui, VIEW, &text, 8);
    let id = d.rows[&2];
    let mut t = Transaction::new(50);
    t.interaction(id, 0, true);
    ui.apply_txn(&t).unwrap();
    ui.dispatch(&craie_ui::events::Event::KeyDown(
        craie_ui::events::KeyInput {
            key: craie_ui::events::Key::Tab,
            text: None,
            char: None,
            mods: Default::default(),
        },
    ));
    assert_eq!(ui.focused(), Some(NodeId(id)));
    scroll(&mut ui, &mut d, 40_000.0);
    assert!(d.rows.contains_key(&2), "focused row kept");
    assert_eq!(d.rows[&2], id);
    assert!(d.rows.len() < 80);
}

/// Incremental equals clean rebuild with lists: seeded splices (insert,
/// remove, replace, above and below the viewport), row text edits, and
/// scrolls, compared at rest against a rebuild from a snapshot.
#[test]
fn list_incremental_equals_rebuild() {
    use craie_core::rng::Rng;
    for seed in [1u64, 2, 3, 4] {
        for scale in [1.0f32, 2.0] {
            let mut rng = Rng::new(seed);
            let (mut ui, mut d) = mount(400, 150.0);
            ui.scale = scale;
            // Item texts, edited in place by the steps.
            let mut texts: Vec<String> = (0..400).map(text).collect();
            // Item identities, parallel to `texts`.
            let mut ids: Vec<u32> = (0..400).collect();
            let mut next_id = 1_000_000;
            let mut now = 0.0;
            d.settle(&mut ui, VIEW, &|i| texts[i as usize].clone(), 8);
            for step in 0..40u64 {
                now += 0.05;
                ui.set_time(now);
                let n = texts.len() as u32;
                let mut t = Transaction::new(10_000 + step);
                match rng.below(4) {
                    0 => {
                        let at = rng.below(n + 1);
                        let rm = rng.below(4).min(n - at);
                        let add: Vec<String> = (0..rng.below(5))
                            .map(|k| text(step as u32 * 13 + k))
                            .collect();
                        let new_ids: Vec<u32> =
                            (0..add.len() as u32).map(|k| next_id + k).collect();
                        next_id += add.len() as u32;
                        let descs: Vec<ItemDesc> = add
                            .iter()
                            .zip(&new_ids)
                            .map(|(s, &id)| ItemDesc {
                                template: 0,
                                text_len: s.chars().count() as u32,
                                id,
                                unchanged: false,
                            })
                            .collect();
                        t.list_splice(LIST, at, rm, &descs);
                        d.spliced(&mut t, at, rm, add.len() as u32);
                        texts.splice(at as usize..(at + rm) as usize, add);
                        ids.splice(at as usize..(at + rm) as usize, new_ids);
                    }
                    1 if !d.rows.is_empty() => {
                        // Edit a rendered row's text (and its description).
                        let k = rng.below(d.rows.len() as u32) as usize;
                        let (&i, &id) = d.rows.iter().nth(k).unwrap();
                        let s = text(i * 31 + step as u32);
                        t.text(id, s.clone(), FONT, 0xFFFF_FFFF);
                        t.list_splice(
                            LIST,
                            i,
                            1,
                            &[ItemDesc {
                                template: 0,
                                text_len: s.chars().count() as u32,
                                id: ids[i as usize],
                                unchanged: false,
                            }],
                        );
                        texts[i as usize] = s;
                    }
                    _ => {
                        let extent = ui.layouts.data(NodeId(SCROLLER)).scroll_extent[1];
                        let y = rng.unit() * extent;
                        ui.scroll_to(NodeId(SCROLLER), 0.0, y);
                    }
                }
                if !t.mutations.is_empty() {
                    ui.apply_txn(&t).unwrap();
                }
                let texts2 = texts.clone();
                d.settle(&mut ui, VIEW, &move |i| texts2[i as usize].clone(), 8);
                if step % 5 == 4 {
                    now += 1.0;
                    ui.set_time(now);
                    ui.settle();
                    ui.render(VIEW);
                    let clean = craie_harness::rebuild(&ui, VIEW);
                    if let Err(m) = craie_harness::compare(&ui, &clean, VIEW, 0.01) {
                        panic!("seed {seed} scale {scale} step {step}: {}", m.0);
                    }
                }
            }
        }
    }
}

/// Assistive technology sees the rendered rows in item order, each with
/// its position among all items.
#[test]
fn rows_report_their_place_in_the_list() {
    let (mut ui, mut d) = mount(5_000, 100.0);
    let mut t = Transaction::new(2);
    t.role(LIST, craie_ui::mutation::Role::List);
    ui.apply_txn(&t).unwrap();
    scroll(&mut ui, &mut d, 20_000.0);
    let tree = ui.a11y_tree(VIEW);
    let (_, list) = tree
        .nodes
        .iter()
        .find(|(id, _)| *id == craie_ui::a11y::aid(NodeId(LIST)))
        .unwrap();
    assert_eq!(list.role(), accesskit::Role::List);
    let kids = list.children();
    assert_eq!(kids.len(), d.rows.len());
    let mut last = 0;
    for k in kids {
        let (_, row) = tree.nodes.iter().find(|(id, _)| id == k).unwrap();
        let pos = row.position_in_set().unwrap();
        assert!(pos > last, "item order");
        last = pos;
        assert_eq!(row.size_of_set(), Some(5_000));
        let index = pos as u32 - 1;
        assert_eq!(*k, craie_ui::a11y::aid(NodeId(d.rows[&index])));
    }
}

/// The range follows paint and hit testing through transforms on the
/// list and on an ancestor between the list and its scroller: every
/// point of the viewport the list covers hits a rendered row.
#[test]
fn transforms_pick_the_visible_rows() {
    use craie_core::geom::Affine;
    const WRAP: u32 = 2;
    // (case, list transform, wrapper transform, scroll offset). Scales
    // apply about the box center; each scroll shows part of the list.
    let cases: [(&str, Option<Affine>, Option<Affine>, f32); 5] = [
        (
            "list up 10k",
            Some(Affine::translate(0.0, -10_000.0)),
            None,
            2_000.0,
        ),
        (
            "wrapper up 8k",
            None,
            Some(Affine::translate(0.0, -8_000.0)),
            2_000.0,
        ),
        (
            "list at half scale",
            Some(Affine::scale(0.5, 0.5)),
            None,
            45_000.0,
        ),
        (
            "wrapper at double scale",
            None,
            Some(Affine::scale(2.0, 2.0)),
            2_000.0,
        ),
        (
            "both",
            Some(Affine::translate(0.0, -3_000.0)),
            Some(Affine::scale(1.5, 1.5)),
            2_000.0,
        ),
    ];
    for (what, on_list, on_wrap, at) in cases {
        let mut ui = Ui::new(2.0);
        let mut d = ListDriver::new(LIST, 100, FONT);
        let mut t = Transaction::new(1);
        t.create(SCROLLER, NodeKind::View)
            .layout(SCROLLER, &scroller_style())
            .append(NIL, SCROLLER);
        t.create(WRAP, NodeKind::View).append(SCROLLER, WRAP);
        t.create(LIST, NodeKind::List)
            .list_config(LIST, 100.0, 20.0, &[d.template()])
            .list_splice(LIST, 0, 0, &(0..3_000).map(desc).collect::<Vec<_>>())
            .append(WRAP, LIST);
        if let Some(m) = on_list {
            t.transform(LIST, m);
        }
        if let Some(m) = on_wrap {
            t.transform(WRAP, m);
        }
        ui.apply_txn(&t).unwrap();
        d.settle(&mut ui, VIEW, &text, 8);
        scroll(&mut ui, &mut d, at);
        let mut hits = 0;
        for k in 0..20 {
            let (x, y) = (
                VIEW.width * 0.5,
                5.0 + k as f32 * (VIEW.height - 10.0) / 19.0,
            );
            let Some(hit) = ui.hit_test(x, y) else {
                continue;
            };
            // The list's own box hit: the list covers the point, and no
            // row does.
            if hit == NodeId(LIST) {
                panic!("{what}: ({x}, {y}) shows the list with no row rendered");
            }
            if ui.host.parent(hit) == NodeId(LIST) {
                hits += 1;
            }
        }
        assert!(hits >= 10, "{what}: only {hits} of 20 points hit rows");
        assert!(d.rows.len() < 120, "{what}: {} rows", d.rows.len());
    }
}

/// A new fallback estimate re-estimates unmeasured items (measurements
/// stay) and matches a clean rebuild.
#[test]
fn fallback_change_reestimates() {
    let (mut ui, mut d) = mount(3_000, 100.0);
    // Template 5 does not exist: every item takes the fallback.
    let mut t = Transaction::new(2);
    t.list_splice(
        LIST,
        0,
        3_000,
        &(0..3_000)
            .map(|i| ItemDesc {
                template: 5,
                ..desc(i)
            })
            .collect::<Vec<_>>(),
    );
    ui.apply_txn(&t).unwrap();
    d.settle(&mut ui, VIEW, &text, 8);
    let measured = ui.host.lists.get(LIST).unwrap().extents.measured_count();
    assert!(measured > 0);
    let mut t = Transaction::new(3);
    t.list_config(LIST, 100.0, 88.0, &[d.template()]);
    ui.apply_txn(&t).unwrap();
    d.settle(&mut ui, VIEW, &text, 8);
    let l = ui.host.lists.get(LIST).unwrap();
    for i in 0..3_000 {
        if !l.extents.is_measured(i) {
            assert_eq!(l.extents.size(i), 88.0, "item {i}");
        }
    }
    assert_eq!(l.extents.measured_count(), measured, "measurements stay");
    ui.set_time(10.0);
    ui.settle();
    ui.render(VIEW);
    let clean = craie_harness::rebuild(&ui, VIEW);
    craie_harness::compare(&ui, &clean, VIEW, 0.01).unwrap_or_else(|m| panic!("{}", m.0));
}

/// Rows layout hides (an index past the end, a duplicate index) are not
/// published to assistive technology, including right after the item
/// count shrinks and before React answers.
#[test]
fn hidden_rows_stay_out_of_semantics() {
    let (mut ui, mut d) = mount(50, 100.0);
    d.settle(&mut ui, VIEW, &text, 8);
    let published = |ui: &Ui| -> Vec<(accesskit::NodeId, usize)> {
        let tree = ui.a11y_tree(VIEW);
        let (_, list) = tree
            .nodes
            .iter()
            .find(|(id, _)| *id == craie_ui::a11y::aid(NodeId(LIST)))
            .unwrap()
            .clone();
        list.children()
            .iter()
            .map(|k| {
                let (_, n) = tree.nodes.iter().find(|(id, _)| id == k).unwrap();
                (*k, n.position_in_set().unwrap())
            })
            .collect()
    };
    let rows = d.rows.len();
    assert_eq!(published(&ui).len(), rows);
    // Shrink to 3 items; the rows past the end stay until React answers.
    let mut t = Transaction::new(40);
    t.list_splice(LIST, 3, 47, &[]);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let p = published(&ui);
    assert_eq!(p.len(), 3, "only rows 0..3 remain published");
    assert!(p.iter().all(|&(_, pos)| pos <= 3));
    // A duplicate index: only the first row with it is published.
    let (a, b) = (d.rows[&0], d.rows[&1]);
    let mut t = Transaction::new(41);
    t.list_index(b, 0);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let p = published(&ui);
    assert_eq!(p.iter().filter(|&&(_, pos)| pos == 1).count(), 1);
    assert!(p.iter().any(|&(k, _)| k == craie_ui::a11y::aid(NodeId(a))));
    assert!(!p.iter().any(|&(k, _)| k == craie_ui::a11y::aid(NodeId(b))));
    // A row with no index is not published either.
    let mut t = Transaction::new(42);
    t.list_index(b, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    assert!(
        !published(&ui)
            .iter()
            .any(|&(k, _)| k == craie_ui::a11y::aid(NodeId(b)))
    );
}

/// Reorders across the viewport, sent as one replacement splice (as the
/// bridge's prefix/suffix diff does): the anchor item keeps its place,
/// its row node, and its measurement; moved rows keep theirs.
#[test]
fn reorders_keep_the_anchor_item() {
    let (mut ui, mut d) = mount(2_000, 100.0);
    d.settle(&mut ui, VIEW, &text, 8);
    scroll(&mut ui, &mut d, 20_000.0);
    let mut order: Vec<u32> = (0..2_000).collect();
    for (step, span) in [(0u64, 5u32), (1, 12)] {
        let l = ui.host.lists.get(LIST).unwrap();
        let anchor = l.item_at(scroll_y(&ui) - ui.layouts.data(NodeId(LIST)).content[1]) as u32;
        let anchor_id = order[anchor as usize];
        let before = row_y(&ui, &d, anchor);
        let node = d.rows[&anchor];
        // Swap (step 0) or reverse (step 1) items around the anchor.
        let (lo, hi) = (anchor - span, anchor + span);
        let mut moved: Vec<u32> = order[lo as usize..=hi as usize].to_vec();
        if step == 0 {
            let last = moved.len() - 1;
            moved.swap(0, last);
        } else {
            moved.reverse();
        }
        // Measured items by identity, before the move.
        let was_measured: Vec<(u32, bool)> = (lo..=hi)
            .map(|i| (order[i as usize], l.extents.is_measured(i as usize)))
            .collect();
        let mut t = Transaction::new(500 + step);
        t.list_splice(
            LIST,
            lo,
            hi - lo + 1,
            // The same items moved: the bridge marks them unchanged.
            &moved
                .iter()
                .map(|&id| ItemDesc {
                    unchanged: true,
                    ..desc(id)
                })
                .collect::<Vec<_>>(),
        );
        let old = order.clone();
        order.splice(lo as usize..=hi as usize, moved.iter().copied());
        let new_pos = |id: u32| order.iter().position(|&x| x == id).unwrap() as u32;
        d.remap(&mut t, &|i| Some(new_pos(old[i as usize])));
        ui.apply_txn(&t).unwrap();
        let texts = order.clone();
        d.settle(&mut ui, VIEW, &move |i| text(texts[i as usize]), 8);
        let now = new_pos(anchor_id);
        let after = row_y(&ui, &d, now);
        assert!(
            (after - before).abs() < 0.01,
            "step {step}: {before} -> {after}"
        );
        assert_eq!(d.rows[&now], node, "step {step}: the same row node");
        let l = ui.host.lists.get(LIST).unwrap();
        for (id, m) in was_measured {
            if m {
                let i = new_pos(id) as usize;
                assert!(
                    l.extents.is_measured(i),
                    "step {step}: item {id} keeps its measurement"
                );
            }
        }
    }
}

/// A focused row survives items inserted above it: the row node, its
/// generation, and focus stay; the next range event names its new index
/// and its identity.
#[test]
fn focus_survives_splices_above() {
    let (mut ui, mut d) = mount(500, 100.0);
    d.settle(&mut ui, VIEW, &text, 8);
    let id = d.rows[&4];
    let generation = ui.host.node(NodeId(id)).unwrap().generation;
    let mut t = Transaction::new(50);
    t.interaction(id, 0, true);
    ui.apply_txn(&t).unwrap();
    ui.dispatch(&craie_ui::events::Event::KeyDown(
        craie_ui::events::KeyInput {
            key: craie_ui::events::Key::Tab,
            text: None,
            char: None,
            mods: Default::default(),
        },
    ));
    assert_eq!(ui.focused(), Some(NodeId(id)));
    ui.render(VIEW);
    ui.take_events();
    let mut t = Transaction::new(51);
    t.list_splice(
        LIST,
        0,
        0,
        &(0..3).map(|i| desc_as(i, 900_000 + i)).collect::<Vec<_>>(),
    );
    d.spliced(&mut t, 0, 0, 3);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let range = ui
        .take_events()
        .into_iter()
        .find(|e| e.kind == craie_ui::events::out_kind::LIST_RANGE)
        .expect("a splice re-reports the range");
    assert_eq!(range.x, 7.0, "focused item's new index");
    assert_eq!(range.key, 4, "focused item's identity");
    assert_eq!(range.y, ui.host.lists.get(LIST).unwrap().revision as f32);
    assert_eq!(ui.focused(), Some(NodeId(id)));
    assert_eq!(ui.host.node(NodeId(id)).unwrap().generation, generation);
}

/// What proves a measurement still valid is the bridge's `unchanged`
/// flag (the same object moved), not equal estimate inputs: an item
/// edited off-window with the same template and length is estimated
/// again; the same item moved keeps its measurement.
#[test]
fn only_unchanged_items_keep_measurements() {
    let (mut ui, mut d) = mount(1_000, 100.0);
    d.settle(&mut ui, VIEW, &text, 8);
    let i = 3u32;
    assert!(
        ui.host
            .lists
            .get(LIST)
            .unwrap()
            .extents
            .is_measured(i as usize)
    );
    // Scroll away: the row unmounts, its measurement stays.
    scroll(&mut ui, &mut d, 30_000.0);
    assert!(!d.rows.contains_key(&i));
    let l = ui.host.lists.get(LIST).unwrap();
    assert!(l.extents.is_measured(i as usize));
    let measured = l.extents.size(i as usize);
    // Edited content, same key, same description: estimated again.
    let mut t = Transaction::new(70);
    t.list_splice(LIST, i, 1, &[desc(i)]);
    ui.apply_txn(&t).unwrap();
    d.settle(&mut ui, VIEW, &text, 8);
    let l = ui.host.lists.get(LIST).unwrap();
    assert!(
        !l.extents.is_measured(i as usize),
        "an edit drops the measurement"
    );
    let _ = measured;
    // Measure it again, then move it unchanged: the measurement stays.
    scroll(&mut ui, &mut d, 0.0);
    let l = ui.host.lists.get(LIST).unwrap();
    assert!(l.extents.is_measured(i as usize));
    let size = l.extents.size(i as usize);
    scroll(&mut ui, &mut d, 30_000.0);
    let mut t = Transaction::new(71);
    t.list_splice(
        LIST,
        i,
        2,
        &[
            desc(i + 1),
            ItemDesc {
                unchanged: true,
                ..desc(i)
            },
        ],
    );
    ui.apply_txn(&t).unwrap();
    d.settle(&mut ui, VIEW, &text, 8);
    let l = ui.host.lists.get(LIST).unwrap();
    assert!(l.extents.is_measured(i as usize + 1), "a move keeps it");
    assert_eq!(l.extents.size(i as usize + 1), size);
}

/// A flipped list (scaleY(-1)): the visually top item is the list's far
/// end. Growing a row visually below it moves nothing, anchored or not
/// (an anchor on the wrong item would move it); growing a row visually
/// above it moves it unless keep-visible holds it. Visual positions come
/// from layout rects and the flip, independent of the anchor code.
#[test]
fn flipped_list_keeps_the_visual_top_item() {
    use craie_core::geom::{Affine, Point};
    for (anchor, above) in [
        (Anchor::KeepVisible, false),
        (Anchor::None, false),
        (Anchor::KeepVisible, true),
        (Anchor::None, true),
    ] {
        let (mut ui, mut d) = mount(300, 200.0);
        let mut t = Transaction::new(2);
        t.transform(LIST, Affine::scale(1.0, -1.0))
            .scroll_anchor(SCROLLER, anchor);
        ui.apply_txn(&t).unwrap();
        d.settle(&mut ui, VIEW, &text, 8);
        scroll(&mut ui, &mut d, 3_000.0);
        // Visual top of row `i`, from layout and the flip about the
        // list's center.
        let visual_top = |ui: &Ui, d: &ListDriver, i: u32| {
            let list = ui.layouts.data(NodeId(LIST));
            let m = Affine::translate(list.rect.origin.x, list.rect.origin.y).mul(
                &Affine::scale(1.0, -1.0).about(Point::new(
                    list.rect.size.width / 2.0,
                    list.rect.size.height / 2.0,
                )),
            );
            let row = ui.layouts.data(NodeId(d.rows[&i])).rect;
            let a = m.apply(Point::new(0.0, row.origin.y)).y;
            let b = m.apply(Point::new(0.0, row.max_y())).y;
            a.min(b) - scroll_y(ui)
        };
        // The row whose visual span holds the viewport top.
        let top = *d
            .rows
            .keys()
            .find(|&&i| {
                let y = visual_top(&ui, &d, i);
                let h = ui.layouts.data(NodeId(d.rows[&i])).rect.size.height;
                y <= 0.0 && y + h > 0.0
            })
            .expect("a row at the visual top");
        let before = visual_top(&ui, &d, top);
        // Flipped: a higher index is visually higher.
        let k = if above { top + 1 } else { top - 2 };
        let id = *d.rows.get(&k).expect("the grown row is rendered");
        let long = format!("{} {}", text(k), text(k + 1));
        let mut t = Transaction::new(3);
        t.text(id, long.clone(), FONT, 0xFFFF_FFFF).list_splice(
            LIST,
            k,
            1,
            &[ItemDesc {
                text_len: long.chars().count() as u32,
                ..desc(k)
            }],
        );
        ui.apply_txn(&t).unwrap();
        let grown = long.clone();
        d.settle(
            &mut ui,
            VIEW,
            &move |i| if i == k { grown.clone() } else { text(i) },
            8,
        );
        let drift = (visual_top(&ui, &d, top) - before).abs();
        let what = format!(
            "{anchor:?}, grown row {}",
            if above { "above" } else { "below" }
        );
        if above && anchor == Anchor::None {
            assert!(drift > 5.0, "{what}: control moves ({drift})");
        } else {
            assert!(drift < 0.01, "{what}: drift {drift}");
        }
    }
}

/// A size probe and the final layout agree on a wide, fractional
/// correction (an estimate of 1e6 measured as 0.01), at an unchanged
/// width and after a width change: the list is as tall as its items and
/// the sibling after it follows.
#[test]
fn probe_and_final_agree_on_wide_corrections() {
    let sibling = 2;
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(SCROLLER, NodeKind::View)
        .layout(SCROLLER, &scroller_style())
        .append(NIL, SCROLLER);
    t.create(LIST, NodeKind::List)
        .list_config(
            LIST,
            0.0,
            20.0,
            &[craie_ui::mutation::ItemTemplate {
                base: 1.0e6,
                inset: 0.0,
                font_size: 0.0,
            }],
        )
        .list_splice(
            LIST,
            0,
            0,
            &[ItemDesc {
                template: 0,
                text_len: 0,
                id: 1,
                unchanged: false,
            }],
        )
        .append(SCROLLER, LIST);
    t.create(sibling, NodeKind::View)
        .layout(
            sibling,
            &taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(10.0),
                    height: taffy::Dimension::length(10.0),
                },
                ..craie_ui::host::default_style()
            },
        )
        .append(SCROLLER, sibling);
    ui.apply_txn(&t).unwrap();
    // Estimates at the width first; then the row renders, 0.01 tall.
    ui.render(VIEW);
    let mut t = Transaction::new(2);
    t.create(100, NodeKind::View)
        .layout(
            100,
            &taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::auto(),
                    height: taffy::Dimension::length(0.01),
                },
                ..craie_ui::host::default_style()
            },
        )
        .list_index(100, 0)
        .append(LIST, 100);
    ui.apply_txn(&t).unwrap();
    for (what, view) in [
        ("same width", VIEW),
        (
            "after a width change",
            Size::new(VIEW.width + 40.0, VIEW.height),
        ),
    ] {
        ui.render(view);
        let total = ui.host.lists.get(LIST).unwrap().total();
        let list = ui.layouts.data(NodeId(LIST)).rect;
        assert!((total - 0.01).abs() < 1e-6, "{what}: total {total}");
        assert!(
            (list.size.height - total).abs() < 1e-6,
            "{what}: list {list:?}"
        );
        let y = ui.layouts.data(NodeId(sibling)).rect.origin.y;
        assert!((y - list.max_y()).abs() < 1e-6, "{what}: sibling at {y}");
    }
}
