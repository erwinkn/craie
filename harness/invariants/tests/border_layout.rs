//! Borders take layout space (D164): the widths a box paints, uniform
//! or per side, inset its content as CSS's `box-sizing: border-box` and
//! React Native do, whatever their color.

use craie_core::geom::{Rect, Size};
use craie_ui::border::BorderSides;
use craie_ui::host::NodeId;
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;

const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};
const RED: u32 = 0xff00_00ff;

fn sized(width: f32, height: f32) -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(width),
            height: taffy::Dimension::length(height),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// Box 1 (100 × 60, a row) in root 0, holding child 2, which grows and
/// stretches to fill it; `paint` styles box 1.
fn boxed(paint: impl FnOnce(&mut Transaction)) -> Ui {
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &sized(400.0, 300.0))
        .append(NIL, 0);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 60.0))
        .append(0, 1);
    let grow = taffy::Style {
        flex_grow: 1.0,
        ..Default::default()
    };
    t.create(2, NodeKind::View)
        .layout(2, &grow)
        .fill(2, 0x0000_ffff)
        .append(1, 2);
    paint(&mut t);
    let mut ui = Ui::new(1.0);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    ui
}

fn rect(ui: &Ui, id: u32) -> Rect {
    ui.layouts.data(NodeId(id)).rect
}

fn sides(widths: [f32; 4], fallback: u8) -> BorderSides {
    BorderSides {
        widths,
        colors: [RED; 4],
        fallback,
    }
}

fn commit(ui: &mut Ui, f: impl FnOnce(&mut Transaction)) {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
}

/// A uniform 4-point border: the child fills the box inside it, and
/// the box's content origin is past it.
#[test]
fn a_border_insets_the_content() {
    let ui = boxed(|t| {
        t.paint(1, None, None, Some((RED, 4.0)));
    });
    assert_eq!(rect(&ui, 2), Rect::new(4.0, 4.0, 92.0, 52.0));
    assert_eq!(ui.layouts.data(NodeId(1)).content, [4.0, 4.0]);
    assert_eq!(rect(&ui, 1).size, Size::new(100.0, 60.0), "border box");
}

/// A divider (`borderBottomWidth: 1`): the child's fill stops above it
/// instead of covering it.
#[test]
fn a_divider_takes_its_row() {
    let ui = boxed(|t| {
        t.border_sides(1, sides([0.0, 0.0, 1.0, 0.0], 0));
    });
    assert_eq!(rect(&ui, 2), Rect::new(0.0, 0.0, 100.0, 59.0));
}

/// Sides that fall back take the uniform width; the others their own.
#[test]
fn fallen_back_sides_take_the_uniform_width() {
    let ui = boxed(|t| {
        t.paint(1, None, None, Some((RED, 5.0)));
        // Bottom 2 of its own; top, right and left fall back (bits 0, 1, 3).
        t.border_sides(1, sides([0.0, 0.0, 2.0, 0.0], 0b1011));
    });
    assert_eq!(rect(&ui, 2), Rect::new(5.0, 5.0, 90.0, 53.0));
}

/// A transparent border still takes its space, as in CSS.
#[test]
fn a_transparent_border_takes_its_space() {
    let ui = boxed(|t| {
        t.paint(1, None, None, Some((0, 3.0)));
    });
    assert_eq!(rect(&ui, 2), Rect::new(3.0, 3.0, 94.0, 54.0));
}

/// An empty box sized by its content is as large as its borders.
#[test]
fn an_empty_box_is_its_borders() {
    let mut t = Transaction::new(1);
    let column = taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        align_items: Some(taffy::AlignItems::START),
        ..sized(400.0, 300.0)
    };
    t.create(0, NodeKind::View)
        .layout(0, &column)
        .append(NIL, 0);
    t.create(1, NodeKind::View)
        .border_sides(1, sides([1.0, 2.0, 3.0, 4.0], 0))
        .append(0, 1);
    let mut ui = Ui::new(1.0);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    assert_eq!(rect(&ui, 1).size, Size::new(6.0, 4.0));
}

/// Changing the paint relays out: a uniform width, sides over it, then
/// sides removed (the uniform border again), then no border.
#[test]
fn border_changes_lay_out_again() {
    let mut ui = boxed(|_| {});
    assert_eq!(rect(&ui, 2), Rect::new(0.0, 0.0, 100.0, 60.0));
    commit(&mut ui, |t| {
        t.paint(1, None, None, Some((RED, 2.0)));
    });
    assert_eq!(rect(&ui, 2), Rect::new(2.0, 2.0, 96.0, 56.0));
    commit(&mut ui, |t| {
        t.border_sides(1, sides([10.0, 0.0, 0.0, 0.0], 0));
    });
    assert_eq!(rect(&ui, 2), Rect::new(0.0, 10.0, 100.0, 50.0));
    commit(&mut ui, |t| {
        t.border_sides(1, BorderSides::default());
    });
    assert_eq!(rect(&ui, 2), Rect::new(2.0, 2.0, 96.0, 56.0));
    commit(&mut ui, |t| {
        t.paint(1, None, None, Some((RED, 0.0)));
    });
    assert_eq!(rect(&ui, 2), Rect::new(0.0, 0.0, 100.0, 60.0));
}

/// The layout style's own border is not read: the border is the one
/// the box paints.
#[test]
fn the_layout_style_border_is_not_read() {
    let ui = boxed(|t| {
        let mut s = sized(100.0, 60.0);
        s.border = taffy::Rect::length(10.0);
        t.layout(1, &s);
    });
    assert_eq!(rect(&ui, 2), Rect::new(0.0, 0.0, 100.0, 60.0));
}
