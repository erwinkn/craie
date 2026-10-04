//! Box shadows through `Ui` (`shadow.rs`): drawn in the box's chunk in
//! CSS order, swapped by variants, cleared with the node, and strict on
//! the wire.

use crate::events::Event;
use crate::geom::{Rect, Size};
use crate::mutation::{Mutation, NodeKind, Transaction};
use crate::shadow::{MAX_SHADOWS, Shadow, Shadows};
use crate::states::{TermDecl, Values, VariantDecl, state_bit, value_field};
use crate::ui::Ui;
use crate::wire;

const NIL: u32 = u32::MAX;
const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};
const FILL: u32 = 0x2020_20FF;

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'static>)) {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

fn rejects(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'static>)) -> bool {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply_txn(&t).is_err()
}

fn sized(w: f32, h: f32) -> taffy::Style {
    taffy::Style {
        flex_shrink: 0.0,
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    }
}

/// The kit's `button`: a ring in the line color, then a soft drop.
fn button() -> [Shadow; 2] {
    [
        Shadow {
            spread: 1.0,
            color: 0x3030_30FF,
            ..Shadow::default()
        },
        Shadow {
            y: 4.0,
            blur: 8.0,
            color: 0x0000_000A,
            ..Shadow::default()
        },
    ]
}

/// `raised`: a deeper drop, no ring.
fn raised() -> [Shadow; 1] {
    [Shadow {
        y: 12.0,
        blur: 24.0,
        color: 0x0000_0014,
        ..Shadow::default()
    }]
}

/// A 100 × 40 box at (20, 20) with radius 6, a fill, and `shadows`.
fn boxed(shadows: &[Shadow]) -> Ui {
    let mut ui = Ui::new(1.0);
    let mut root = sized(400.0, 300.0);
    root.padding = taffy::Rect::length(20.0);
    apply(&mut ui, |t| {
        t.create(0, NodeKind::View).layout(0, &root).append(NIL, 0);
        t.create(1, NodeKind::View)
            .layout(1, &sized(100.0, 40.0))
            .paint(1, Some(FILL), Some(6.0), None)
            .shadows(1, shadows)
            .append(0, 1);
    });
    ui.render(VIEW);
    ui
}

/// The drawn primitives: (kind, device bounds), in draw order.
fn drawn(ui: &Ui) -> Vec<(u8, Rect)> {
    ui.scene()
        .resolve(&|r| r.0 as u64)
        .iter()
        .map(|r| (r.kind, r.bounds))
        .collect()
}

/// Outer shadows draw under the fill, the last listed lowest; inset
/// ones over it. The ring is the box grown by its spread; the drop is
/// the box moved down; the inset shadow is cut to the padding box.
#[test]
fn shadows_draw_around_the_box_in_css_order() {
    let inset = Shadow {
        y: 1.0,
        blur: 2.0,
        color: 0x0000_001F,
        inset: true,
        ..Shadow::default()
    };
    let [ring, drop] = button();
    let ui = boxed(&[ring, drop, inset]);
    assert_eq!(
        drawn(&ui),
        vec![
            (3, Rect::new(20.0, 24.0, 100.0, 40.0)),
            (3, Rect::new(19.0, 19.0, 102.0, 42.0)),
            (0, Rect::new(20.0, 20.0, 100.0, 40.0)),
            (3, Rect::new(20.0, 21.0, 100.0, 40.0)),
        ]
    );
    // The chunk's bounds reach the drop's blur (3 σ, and a pixel).
    let b = ui.scene().chunk(1).unwrap().bounds;
    assert_eq!(b.max_y(), 4.0 + 40.0 + 3.0 * 4.0 + 1.0);
    // Transparent shadows draw nothing.
    let clear = Shadow {
        blur: 4.0,
        ..Shadow::default()
    };
    let ui = boxed(&[clear]);
    assert_eq!(drawn(&ui).len(), 1);
}

/// A variant swaps the whole list (`_hover: { elevation: raised }`) and
/// rebuilds the chunk; leaving restores the base. A tabled node's own
/// `SHADOWS` sets the base.
#[test]
fn variants_swap_shadows() {
    let mut ui = boxed(&button());
    let hover = VariantDecl {
        terms: vec![TermDecl {
            scope: 1,
            mask: state_bit::HOVER,
        }],
        values: Values {
            mask: value_field::SHADOWS,
            shadows: Shadows::new(&raised()).unwrap(),
            ..Values::default()
        },
        ..Default::default()
    };
    apply(&mut ui, |t| {
        t.states(1, 0).variants(1, &[hover]);
    });
    ui.render(VIEW);
    let shadows = |ui: &Ui| ui.host.shadows.get(&1).map(|s| s.as_slice().to_vec());
    assert_eq!(shadows(&ui), Some(button().to_vec()));
    ui.dispatch(&Event::PointerMove { x: 30.0, y: 30.0 });
    ui.render(VIEW);
    assert_eq!(shadows(&ui), Some(raised().to_vec()));
    assert_eq!(drawn(&ui)[0], (3, Rect::new(20.0, 32.0, 100.0, 40.0)));
    // Its own ops set the base under the variant.
    apply(&mut ui, |t| {
        t.shadows(1, &[]);
    });
    ui.render(VIEW);
    assert_eq!(shadows(&ui), Some(raised().to_vec()), "the variant holds");
    ui.dispatch(&Event::PointerMove { x: 300.0, y: 200.0 });
    ui.render(VIEW);
    assert_eq!(shadows(&ui), None, "the new base: none");
    assert_eq!(drawn(&ui).len(), 1);
}

/// A recycled id starts without shadows.
#[test]
fn a_new_occupant_has_no_shadows() {
    let mut ui = boxed(&button());
    apply(&mut ui, |t| {
        t.detach(1).remove(1);
        t.create(1, NodeKind::View)
            .layout(1, &sized(100.0, 40.0))
            .fill(1, FILL)
            .append(0, 1);
    });
    ui.render(VIEW);
    assert!(ui.host.shadows.is_empty());
    assert_eq!(drawn(&ui).len(), 1);
}

/// Shadows cross the wire in `PAINT` and in variant values; decoding is
/// strict on the count and the flags, validation on the numbers and
/// the node.
#[test]
fn shadows_cross_the_wire_and_validate() {
    let mut inset = raised();
    inset[0].inset = true;
    let values = Values {
        mask: value_field::SHADOWS | value_field::FILL,
        fill: FILL,
        shadows: Shadows::new(&inset).unwrap(),
        ..Values::default()
    };
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::View)
        .append(NIL, 1)
        .shadows(1, &button())
        .states(1, 0)
        .variants(
            1,
            &[VariantDecl {
                terms: vec![TermDecl {
                    scope: 1,
                    mask: state_bit::HOVER,
                }],
                values,
                ..Default::default()
            }],
        );
    let buf = wire::encode(&t);
    assert_eq!(wire::decode(&buf).unwrap().mutations, t.mutations);

    // A count past the cap, then unknown flags.
    let one = |count: u8, flags: u8| {
        let mut t = Transaction::new(1);
        t.shadows(1, &raised());
        let mut buf = wire::encode(&t);
        let at = buf
            .windows(2)
            .position(|w| w == [wire::paint_field::SHADOWS, 1])
            .unwrap();
        buf[at + 1] = count;
        let last = buf.len() - 1;
        buf[last] = flags;
        wire::decode(&buf).map(|_| ())
    };
    assert!(one(1, 0).is_ok());
    assert!(one(MAX_SHADOWS as u8 + 1, 0).is_err());
    assert!(one(1, 2).is_err());

    let mut ui = boxed(&[]);
    let bad = |s: Shadow| Mutation::Paint {
        id: 1,
        fill: None,
        radius: None,
        border: None,
        shadows: Shadows::new(&[s]),
    };
    for s in [
        Shadow {
            blur: -1.0,
            ..Shadow::default()
        },
        Shadow {
            x: f32::NAN,
            ..Shadow::default()
        },
        Shadow {
            spread: 1e6,
            ..Shadow::default()
        },
    ] {
        assert!(rejects(&mut ui, |t| {
            t.push(bad(s));
        }));
    }
    apply(&mut ui, |t| {
        t.create(2, NodeKind::Text)
            .text(2, "hi", 14.0, FILL)
            .append(0, 2);
    });
    assert!(rejects(&mut ui, |t| {
        t.shadows(2, &raised());
    }));
}
