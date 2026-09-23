//! The compact layout row must not change layout. Each test sends the
//! original `taffy::Style` values through `Ui::apply_txn`, which stores
//! them as `LayoutRow`s, and compares the result with a `TaffyTree` that
//! holds the same original styles. The reference never reads the row, so
//! a field that the row loses shows up as a difference.
//!
//! Leaves are empty views. Their measure is zero on both sides, so the
//! test does not depend on text or vector measurement.

use craie_core::geom::{Rect, Size};
use craie_core::rng::Rng;
use craie_ui::host::NodeId;
use craie_ui::layout::LayoutData;
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;
use taffy::{
    AlignContent, AlignItems, AlignmentSafety, AvailableSpace, BoxSizing, Contain, Dimension,
    Direction, Display, FlexDirection, FlexWrap, Layout, LengthPercentage, LengthPercentageAuto,
    Overflow, Point, Position, Style, TaffyTree,
};

const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};

/// A generated tree: `styles[i]` and `parents[i]` describe node id `i + 1`.
/// Node 1 is the root.
struct Tree {
    styles: Vec<Style>,
    parents: Vec<u32>,
}

#[test]
fn layout_row_matches_original_styles() {
    let mut trees = 0;
    for seed in 1..=300u64 {
        let mut rng = Rng::new(seed);
        let tree = gen_tree(&mut rng);
        assert_same_layout(&tree, &format!("seed {seed}"));
        trees += 1;
    }
    assert_eq!(trees, 300);
}

/// S6A-01: a safe center must not push an overflowing item past the
/// start edge.
#[test]
fn safe_alignment_keeps_overflow_at_the_start() {
    let mut root = row_root();
    root.align_items = Some(AlignItems::SAFE_CENTER);
    root.justify_content = Some(AlignContent::SAFE_CENTER);
    let child = sized(200.0, 200.0);
    let ui = assert_same_layout(&pair(root, child), "safe center");
    let rect = ui.layouts.data(NodeId(2)).rect;
    assert_eq!((rect.origin.x, rect.origin.y), (0.0, 0.0));
}

/// S6A-02: a row with `direction: rtl` starts at the right edge.
#[test]
fn rtl_rows_start_at_the_right() {
    let mut root = row_root();
    root.direction = Direction::Rtl;
    let ui = assert_same_layout(&pair(root, sized(20.0, 20.0)), "rtl");
    assert_eq!(ui.layouts.data(NodeId(2)).rect.origin.x, 80.0);
}

/// S6A-03: a scroll container reserves its scrollbar width.
#[test]
fn scrollbar_width_narrows_the_content() {
    let mut root = row_root();
    root.flex_direction = FlexDirection::Column;
    root.overflow = Point {
        x: Overflow::Scroll,
        y: Overflow::Scroll,
    };
    root.scrollbar_width = 15.0;
    let child = Style {
        size: taffy::Size {
            width: Dimension::auto(),
            height: Dimension::length(20.0),
        },
        ..Style::default()
    };
    let ui = assert_same_layout(&pair(root, child), "scrollbar");
    assert_eq!(ui.layouts.data(NodeId(2)).rect.size.width, 85.0);
}

fn row_root() -> Style {
    Style {
        size: taffy::Size {
            width: Dimension::length(100.0),
            height: Dimension::length(100.0),
        },
        ..Style::default()
    }
}

fn sized(width: f32, height: f32) -> Style {
    Style {
        flex_shrink: 0.0,
        size: taffy::Size {
            width: Dimension::length(width),
            height: Dimension::length(height),
        },
        ..Style::default()
    }
}

fn pair(root: Style, child: Style) -> Tree {
    Tree {
        styles: vec![root, child],
        parents: vec![NIL, 1],
    }
}

/// Lays out `tree` through `Ui` and through a reference `TaffyTree`, and
/// requires the same bits for every node.
fn assert_same_layout(tree: &Tree, label: &str) -> Ui {
    let mut t = Transaction::new(1);
    for (i, (style, parent)) in tree.styles.iter().zip(&tree.parents).enumerate() {
        let id = i as u32 + 1;
        t.create(id, NodeKind::View)
            .layout(id, style)
            .append(*parent, id);
    }
    let mut ui = Ui::new(2.0);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);

    let mut reference: TaffyTree = TaffyTree::new();
    reference.disable_rounding();
    let nodes: Vec<taffy::NodeId> = tree
        .styles
        .iter()
        .map(|style| reference.new_leaf(style.clone()).unwrap())
        .collect();
    for (i, parent) in tree.parents.iter().enumerate().skip(1) {
        reference
            .add_child(nodes[*parent as usize - 1], nodes[i])
            .unwrap();
    }
    reference
        .compute_layout(
            nodes[0],
            taffy::Size {
                width: AvailableSpace::Definite(VIEW.width),
                height: AvailableSpace::Definite(VIEW.height),
            },
        )
        .unwrap();
    for (i, node) in nodes.iter().enumerate() {
        let id = NodeId(i as u32 + 1);
        let want = data_from_taffy(reference.layout(*node).unwrap());
        assert_data_eq(ui.layouts.data(id), want, id, label);
    }
    ui
}

fn gen_tree(rng: &mut Rng) -> Tree {
    let count = 2 + rng.below(24) as usize;
    let mut styles = vec![gen_style(rng)];
    let mut parents = vec![NIL];
    for i in 1..count {
        styles.push(gen_style(rng));
        parents.push(1 + rng.below(i as u32));
    }
    Tree { styles, parents }
}

fn gen_style(rng: &mut Rng) -> Style {
    Style {
        display: pick(
            rng,
            &[Display::Flex, Display::Flex, Display::Flex, Display::None],
        ),
        position: pick(
            rng,
            &[Position::Relative, Position::Relative, Position::Absolute],
        ),
        direction: pick(rng, &[Direction::Ltr, Direction::Rtl]),
        box_sizing: pick(rng, &[BoxSizing::BorderBox, BoxSizing::ContentBox]),
        overflow: Point {
            x: gen_overflow(rng),
            y: gen_overflow(rng),
        },
        scrollbar_width: pick(rng, &[0.0, 0.0, 6.0, 15.0]),
        contain: pick(
            rng,
            &[
                Contain::NONE,
                Contain::NONE,
                Contain::LAYOUT,
                Contain::PAINT,
                Contain::CONTENT,
            ],
        ),
        flex_direction: pick(
            rng,
            &[
                FlexDirection::Row,
                FlexDirection::Column,
                FlexDirection::RowReverse,
                FlexDirection::ColumnReverse,
            ],
        ),
        flex_wrap: pick(
            rng,
            &[FlexWrap::NoWrap, FlexWrap::Wrap, FlexWrap::WrapReverse],
        ),
        justify_content: gen_content(rng),
        align_content: gen_content(rng),
        align_items: gen_items(rng),
        align_self: gen_items(rng),
        gap: taffy::Size {
            width: gen_lp(rng),
            height: gen_lp(rng),
        },
        size: taffy::Size {
            width: gen_dim(rng),
            height: gen_dim(rng),
        },
        min_size: taffy::Size {
            width: gen_lpa(rng),
            height: gen_lpa(rng),
        },
        max_size: taffy::Size {
            width: gen_lpa(rng),
            height: gen_lpa(rng),
        },
        aspect_ratio: pick(rng, &[None, None, Some(0.5), Some(1.0), Some(16.0 / 9.0)]),
        margin: taffy::Rect {
            left: gen_lpa(rng),
            right: gen_lpa(rng),
            top: gen_lpa(rng),
            bottom: gen_lpa(rng),
        },
        padding: taffy::Rect {
            left: gen_lp(rng),
            right: gen_lp(rng),
            top: gen_lp(rng),
            bottom: gen_lp(rng),
        },
        border: taffy::Rect {
            left: gen_lp(rng),
            right: gen_lp(rng),
            top: gen_lp(rng),
            bottom: gen_lp(rng),
        },
        inset: taffy::Rect {
            left: gen_lpa(rng),
            right: gen_lpa(rng),
            top: gen_lpa(rng),
            bottom: gen_lpa(rng),
        },
        flex_basis: gen_dim(rng),
        flex_grow: pick(rng, &[0.0, 0.0, 1.0, 2.5]),
        flex_shrink: pick(rng, &[0.0, 1.0, 1.0, 3.0]),
        ..Style::default()
    }
}

fn pick<T: Clone>(rng: &mut Rng, values: &[T]) -> T {
    values[rng.below(values.len() as u32) as usize].clone()
}

fn gen_length(rng: &mut Rng) -> f32 {
    pick(rng, &[0.0, 4.0, 10.0, 37.5, 120.0, -8.0])
}

fn gen_percent(rng: &mut Rng) -> f32 {
    pick(rng, &[0.0, 0.1, 0.25, 0.5, 1.0])
}

fn gen_lp(rng: &mut Rng) -> LengthPercentage {
    match rng.below(4) {
        0 => LengthPercentage::percent(gen_percent(rng)),
        _ => LengthPercentage::length(gen_length(rng).max(0.0)),
    }
}

fn gen_lpa(rng: &mut Rng) -> LengthPercentageAuto {
    match rng.below(4) {
        0 => LengthPercentageAuto::percent(gen_percent(rng)),
        1 => LengthPercentageAuto::length(gen_length(rng)),
        _ => LengthPercentageAuto::auto(),
    }
}

fn gen_dim(rng: &mut Rng) -> Dimension {
    match rng.below(6) {
        0 => Dimension::percent(gen_percent(rng)),
        1 | 2 => Dimension::length(gen_length(rng).max(0.0)),
        _ => Dimension::auto(),
    }
}

fn gen_overflow(rng: &mut Rng) -> Overflow {
    pick(
        rng,
        &[
            Overflow::Visible,
            Overflow::Visible,
            Overflow::Clip,
            Overflow::Hidden,
            Overflow::Scroll,
        ],
    )
}

fn gen_safety(rng: &mut Rng) -> AlignmentSafety {
    pick(rng, &[AlignmentSafety::Unsafe, AlignmentSafety::Safe])
}

fn gen_items(rng: &mut Rng) -> Option<AlignItems> {
    let value = pick(
        rng,
        &[
            None,
            Some(AlignItems::START),
            Some(AlignItems::END),
            Some(AlignItems::FLEX_START),
            Some(AlignItems::FLEX_END),
            Some(AlignItems::SELF_START),
            Some(AlignItems::SELF_END),
            Some(AlignItems::CENTER),
            Some(AlignItems::BASELINE),
            Some(AlignItems::STRETCH),
        ],
    );
    value.map(|mut value| {
        value.safety = gen_safety(rng);
        value
    })
}

fn gen_content(rng: &mut Rng) -> Option<AlignContent> {
    let value = pick(
        rng,
        &[
            None,
            Some(AlignContent::START),
            Some(AlignContent::END),
            Some(AlignContent::FLEX_START),
            Some(AlignContent::FLEX_END),
            Some(AlignContent::CENTER),
            Some(AlignContent::STRETCH),
            Some(AlignContent::SPACE_BETWEEN),
            Some(AlignContent::SPACE_EVENLY),
            Some(AlignContent::SPACE_AROUND),
        ],
    );
    value.map(|mut value| {
        value.safety = gen_safety(rng);
        value
    })
}

fn data_from_taffy(layout: &Layout) -> LayoutData {
    let rect = Rect::new(
        layout.location.x,
        layout.location.y,
        layout.size.width,
        layout.size.height,
    );
    let content = [
        layout.border.left + layout.padding.left,
        layout.border.top + layout.padding.top,
    ];
    let insets = [
        layout.border.left + layout.border.right + layout.padding.left + layout.padding.right,
        layout.border.top + layout.border.bottom + layout.padding.top + layout.padding.bottom,
    ];
    let clip_box = Rect::new(
        layout.border.left,
        layout.border.top,
        (layout.size.width - layout.border.left - layout.border.right).max(0.0),
        (layout.size.height - layout.border.top - layout.border.bottom).max(0.0),
    );
    let so = &layout.scrollable_overflow_rect;
    let scroll_extent = [
        (so.right - clip_box.size.width).max(0.0),
        (so.bottom - clip_box.size.height).max(0.0),
    ];
    LayoutData {
        rect,
        content,
        insets,
        clip_box,
        scroll_extent,
    }
}

fn assert_data_eq(got: LayoutData, want: LayoutData, id: NodeId, label: &str) {
    assert_rect_eq(got.rect, want.rect, "rect", id, label);
    assert_pair_eq(got.content, want.content, "content", id, label);
    assert_pair_eq(got.insets, want.insets, "insets", id, label);
    assert_rect_eq(got.clip_box, want.clip_box, "clip", id, label);
    assert_pair_eq(
        got.scroll_extent,
        want.scroll_extent,
        "scroll_extent",
        id,
        label,
    );
}

fn assert_rect_eq(got: Rect, want: Rect, field: &str, id: NodeId, label: &str) {
    assert_bits(got.origin.x, want.origin.x, field, id, label);
    assert_bits(got.origin.y, want.origin.y, field, id, label);
    assert_bits(got.size.width, want.size.width, field, id, label);
    assert_bits(got.size.height, want.size.height, field, id, label);
}

fn assert_pair_eq(got: [f32; 2], want: [f32; 2], field: &str, id: NodeId, label: &str) {
    assert_bits(got[0], want[0], field, id, label);
    assert_bits(got[1], want[1], field, id, label);
}

fn assert_bits(got: f32, want: f32, field: &str, id: NodeId, label: &str) {
    assert_eq!(
        got.to_bits(),
        want.to_bits(),
        "{field}: node {} {label}: got {got:?}, want {want:?}",
        id.0
    );
}
