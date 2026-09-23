//! Cross-language wire verification: `packages/bridge/test/fixture.bin`
//! is produced by the TypeScript encoder (scripts/gen-fixture.ts) and must
//! decode and execute cleanly here.

use craie_core::Affine;
use craie_ui::host::NodeId;
use craie_ui::mutation::{Mutation, NodeKind, Role};
use craie_ui::surface;
use craie_ui::ui::Ui;
use craie_ui::wire;
use taffy::{AlignContent, AlignItems, Dimension, FlexDirection, LengthPercentage, Position};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packages/bridge/test/fixture.bin"
);

#[test]
fn js_fixture_decodes_and_executes() {
    let buf = std::fs::read(FIXTURE).expect("run `bun packages/bridge/scripts/gen-fixture.ts`");
    let txn = wire::decode(&buf).expect("fixture must decode");
    assert_eq!(txn.seq, 99);
    // Identical styles intern to one table row; the span table holds
    // the paragraph's two spans.
    assert_eq!(txn.styles.len(), 1);
    assert_eq!(txn.spans.len(), 2);

    let mut ui = Ui::new(1.0);
    assert_eq!(ui.apply(&buf).unwrap(), 99);
    let host = &ui.host;

    // remove(1): view + input + surface + list + row remain.
    assert_eq!(host.len(), 5);
    assert_eq!(host.kind(NodeId(0)), Some(NodeKind::View));
    let paint = host.paint[0];
    assert_eq!(paint.fill, 0x1122_33ff);
    assert_eq!(paint.radius, 6.5);
    assert_eq!((paint.border_color, paint.border_width), (0xff00_00ff, 2.0));

    // Spatial: translateX(3) · scale(2), opacity 0.75.
    let s = host.spatial[0];
    assert_eq!(s.transform, Affine([2.0, 0.0, 0.0, 2.0, 3.0, 0.0]));
    assert_eq!(s.opacity, 0.75);

    // Input: listeners, focusable, explicit role, config.
    assert_eq!(host.kind(NodeId(2)), Some(NodeKind::Input));
    let i = host.interaction(NodeId(2));
    assert_eq!(i.listeners, 0x1ff);
    assert!(i.focusable);
    assert_eq!(i.role, Role::MultilineTextInput);
    assert_eq!(ui.inputs.text(2), "seed");

    // The root owns its layout row.
    let style = host.style(NodeId(0));
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

    // Text: created, styled with two spans, moved, removed.
    assert!(host.node(NodeId(1)).is_none());
    let para = txn
        .mutations
        .iter()
        .find_map(|m| match m {
            Mutation::Paragraph { text, spans, .. } => Some((text.clone(), spans.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(para.0, "héllo — مرحبا 日本語");
    let spans = &txn.spans[para.1.start as usize..para.1.end as usize];
    assert_eq!(spans[1].start as usize, "héllo ".len());
    assert!(spans[1].italic);
    assert_eq!(spans[1].weight, 700);

    // Surface: kind, params, payload bytes.
    let sd = &host.surfaces[&3];
    assert_eq!(sd.kind, surface::kind::BARS);
    assert_eq!(sd.params[0], 0x6dc7_c8ff);
    let values: Vec<f32> = surface::payload_f32(&sd.payload).collect();
    assert_eq!(values, [0.25, 0.5, 0.75, 1.0]);

    // Labels: set, then cleared.
    assert_eq!(host.label(NodeId(0)), Some("root container"));
    assert_eq!(host.label(NodeId(3)), None);
    assert_eq!(host.children(NodeId(0)), [NodeId(3), NodeId(2), NodeId(4)]);

    // List: templates, items after two splices, a row, an anchor.
    let l = host.lists.get(4).unwrap();
    assert_eq!((l.overscan, l.fallback), (250.0, 36.0));
    assert_eq!(l.templates[0].inset, 16.0);
    assert_eq!(l.templates[1].base, 48.0);
    assert_eq!(l.templates[1].font_size, 0.0);
    assert_eq!(l.len(), 2);
    assert_eq!(l.descs[0].text_len, 42);
    assert_eq!(l.descs[1].text_len, 70_000);
    assert_eq!(host.list_index[5], 1);
    assert_eq!(host.lists.policy(0), craie_ui::mutation::Anchor::StickToEnd);
}
