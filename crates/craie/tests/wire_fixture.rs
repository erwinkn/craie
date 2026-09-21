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

    // The fixture ends with remove(1): one live view remains.
    assert_eq!(host.len(), 1);
    let root = host.node(NodeId(0)).expect("root view");
    assert_eq!(root.kind(), NodeKind::VIEW);
    assert_eq!(host.view(NodeId(0)).unwrap().color, 0x1b1d_24ff);

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
}
