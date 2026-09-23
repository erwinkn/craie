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

fn desc(i: u32) -> ItemDesc {
    ItemDesc {
        template: 0,
        text_len: text(i).chars().count() as u32,
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
    assert!((list_h - l.extents.total()).abs() < 1e-2, "{list_h}");
    for (&i, &id) in &d.rows {
        let y = ui.layouts.data(NodeId(id)).rect.origin.y;
        assert!((y - l.extents.offset(i as usize)).abs() < 1e-3, "row {i}");
        assert!(l.extents.is_measured(i as usize));
        assert_eq!(
            ui.layouts.data(NodeId(id)).rect.size.height,
            l.extents.size(i as usize)
        );
    }
    // The scroller can reach the end of all items.
    let extent = ui.layouts.data(NodeId(SCROLLER)).scroll_extent[1];
    assert!((extent - (l.extents.total() - VIEW.height)).abs() < 1e-2);
}

/// Scrolls to `y` and lets the list render its new range.
fn scroll(ui: &mut Ui, d: &mut ListDriver, y: f32) {
    ui.scroll_to(NodeId(SCROLLER), 0.0, y);
    d.settle(ui, VIEW, &text, 8);
}

/// The oracle: once every item has rendered (and been measured), each
/// item's offset equals its row's position in a plain column holding
/// every row, and the list is exactly as tall. At 1x and 2x.
#[test]
fn virtualized_equals_plain_column() {
    let n = 300;
    let (mut ui, mut d) = mount(n, 100.0);
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
        "every item measured"
    );

    let mut plain = Ui::new(2.0);
    let mut t = Transaction::new(1);
    t.create(SCROLLER, NodeKind::View)
        .layout(SCROLLER, &scroller_style())
        .append(NIL, SCROLLER);
    t.create(LIST, NodeKind::View).append(SCROLLER, LIST);
    for i in 0..n {
        t.create(100 + i, NodeKind::Text)
            .text(100 + i, text(i), FONT, 0xFFFF_FFFF)
            .append(LIST, 100 + i);
    }
    plain.apply_txn(&t).unwrap();
    plain.render(VIEW);
    for i in 0..n {
        let want = plain.layouts.data(NodeId(100 + i)).rect.origin.y;
        let got = l.extents.offset(i as usize);
        assert!((got - want).abs() < 1e-2, "item {i}: {got} vs {want}");
    }
    let want = plain.layouts.data(NodeId(LIST)).rect.size.height;
    assert!((ui.layouts.data(NodeId(LIST)).rect.size.height - want).abs() < 1e-2);
    // Rendered rows are laid out exactly like their plain twins.
    for (&i, &id) in &d.rows {
        let a = ui.layouts.data(NodeId(id)).rect;
        let b = plain.layouts.data(NodeId(100 + i)).rect;
        assert!(
            (a.origin.y - b.origin.y).abs() < 1e-2 && a.size == b.size,
            "row {i}"
        );
    }
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
    let top = l.extents.index_at(scroll_y(&ui)) as u32;
    assert!(d.rows.contains_key(&top), "the top visible item renders");
    assert!(d.rows.len() < 80);
}

/// Screen y of item `i` from the list's extents (rendered or not).
fn item_y(ui: &Ui, i: u32) -> f32 {
    let l = ui.host.lists.get(LIST).unwrap();
    ui.layouts.data(NodeId(LIST)).rect.origin.y + l.extents.offset(i as usize) - scroll_y(ui)
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
        let added: Vec<ItemDesc> = (0..50).map(|i| desc(i + 7)).collect();
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
                        let descs: Vec<ItemDesc> = add
                            .iter()
                            .map(|s| ItemDesc {
                                template: 0,
                                text_len: s.chars().count() as u32,
                            })
                            .collect();
                        t.list_splice(LIST, at, rm, &descs);
                        d.spliced(&mut t, at, rm, add.len() as u32);
                        texts.splice(at as usize..(at + rm) as usize, add);
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
