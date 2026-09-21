//! Cross-language wire verification: `packages/bridge/test/fixture.bin`
//! is produced by the TypeScript encoder (scripts/gen-fixture.ts) and must
//! decode + apply cleanly here.

use craie::host::{Host, NodeId, NodeKind};
use craie::layout::Layouts;
use craie::wire::{self, Op};
use taffy::{AlignContent, AlignItems, Dimension, FlexDirection, LengthPercentage, Position};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packages/bridge/test/fixture.bin"
);

#[test]
fn js_fixture_decodes() {
    let buf = std::fs::read(FIXTURE).expect("run `bun packages/bridge/scripts/gen-fixture.ts`");
    let txn = wire::decode(&buf).expect("fixture must decode");
    assert_eq!(txn.seq, 99);

    let mut host = Host::new();
    let mut layouts = Layouts::new();
    txn.apply(&mut host, &mut layouts).unwrap();

    // The fixture ends with remove(1): view + input + custom remain live.
    assert_eq!(host.len(), 3);
    let root = host.node(NodeId(0)).expect("root view");
    assert_eq!(root.kind(), NodeKind::VIEW);
    // The masked PAINT op overwrote viewPaint's fill and added
    // radius + border.
    let paint = host.view(NodeId(0)).unwrap();
    assert_eq!(paint.color, 0x1122_33ff);
    assert_eq!(paint.radius, 6.5);
    assert_eq!((paint.border_color, paint.border_w), (0xff00_00ff, 2.0));

    // The input node: props + listeners + focusable decoded.
    let input = host.node(NodeId(2)).expect("input node");
    assert_eq!(input.kind(), NodeKind::INPUT);
    let props = host.props(NodeId(2));
    assert_eq!(props.listeners, 0x1ff);
    assert!(props.focusable);

    // Style id 0 was defined by the txn and applied to the root.
    let style = layouts.style(craie::host::StyleId(root.style));
    assert_eq!(style.display, taffy::Display::Flex);
    assert_eq!(style.flex_direction, FlexDirection::Column);
    assert_eq!(style.gap.width, LengthPercentage::length(12.0));
    assert_eq!(style.padding.left, LengthPercentage::length(16.0));
    assert_eq!(style.padding.top, LengthPercentage::percent(0.10));
    assert_eq!(style.size.width, Dimension::percent(0.5));
    assert_eq!(style.size.height, Dimension::auto());
    assert_eq!(style.position, Position::Absolute);
    assert_eq!(style.inset.left, taffy::LengthPercentageAuto::length(4.0));
    assert_eq!(style.margin.top, taffy::LengthPercentageAuto::auto());
    assert_eq!(style.align_items, Some(AlignItems::CENTER));
    assert_eq!(style.justify_content, Some(AlignContent::SPACE_BETWEEN));
    assert_eq!(style.flex_grow, 1.5);
    assert_eq!(style.flex_shrink, 0.5);
    assert_eq!(style.aspect_ratio, Some(1.25));
    assert_eq!(style.overflow.x, taffy::Overflow::Hidden);

    // Text was created, styled, hidden/unhidden, detached, re-placed, removed.
    assert!(host.node(NodeId(1)).is_none());
    assert!(txn.ops.iter().any(|op| matches!(
        op,
        Op::SetText { text, .. } if *text == "héllo — مرحبا 日本語"
    )));

    // The custom node decoded its payload op.
    assert_eq!(host.node(NodeId(3)).unwrap().kind(), NodeKind::CUSTOM);
    assert!(txn.ops.iter().any(|op| matches!(
        op,
        Op::Custom { id: 3, tag: 7, data, text }
            if *data == [0.25, 0.5, 0.75, 1.0] && *text == "0.1,0.4,0.9"
    )));

    // Accessibility labels decoded (set, then cleared).
    assert!(txn.ops.iter().any(|op| matches!(
        op,
        Op::Label { id: 0, text } if *text == "root container"
    )));
    assert!(txn
        .ops
        .iter()
        .any(|op| matches!(op, Op::Label { id: 3, text } if text.is_empty())));
}
