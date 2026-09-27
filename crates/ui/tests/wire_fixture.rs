//! Cross-language wire verification: `packages/bridge/test/fixture.bin`
//! is produced by the TypeScript encoder (scripts/gen-fixture.ts) and must
//! decode and execute cleanly here.

use craie_core::Affine;
use craie_ui::claims::{Claim, chord_flag, claim_kind};
use craie_ui::events::{Key, Mods};
use craie_ui::host::NodeId;
use craie_ui::input::SubmitKey;
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
    // Span fields: span zero's line height; span one's family (through
    // the string table), both decorations, letter spacing, weight, italic.
    let (s0, s1) = (txn.spans[0], txn.spans[1]);
    assert_eq!((s0.line_height, s0.family), (24.0, craie_ui::mutation::NIL));
    assert!(s0.inherit_color && !s1.inherit_color);
    assert_eq!(txn.families[s1.family as usize], "monospace");
    assert_eq!((s1.decoration, s1.letter_spacing), (3, 0.5));
    assert_eq!((s1.weight, s1.italic), (700, true));

    let mut ui = Ui::new(1.0);
    assert_eq!(ui.apply(&buf).unwrap(), 99);
    let host = &ui.host;

    // remove(1): view + input + surface + list + row + two vectors +
    // layer remain.
    assert_eq!(host.len(), 8);
    assert_eq!(host.kind(NodeId(0)), Some(NodeKind::View));
    let paint = host.paint[0];
    assert_eq!(paint.fill, 0x1122_33ff);
    assert_eq!(paint.radius, 6.5);
    assert_eq!((paint.border_color, paint.border_width), (0xff00_00ff, 2.0));

    // Spatial: translateX(3) · scale(2), opacity 0.75, z -2.
    let s = host.spatial[0];
    assert_eq!(s.transform, Affine([2.0, 0.0, 0.0, 2.0, 3.0, 0.0]));
    assert_eq!(s.opacity, 0.75);
    assert_eq!(s.z, -2);

    // A layer container owned from inside the root, with z alone.
    assert_eq!(host.owners[&7], 6);
    assert_eq!(host.spatial[7].z, 50);
    assert_eq!(host.spatial[7].opacity, 1.0);
    assert_eq!(host.paint_order(NodeId::NIL)[..], [NodeId(0), NodeId(7)]);

    // Input: listeners, focusable, explicit role, config.
    assert_eq!(host.kind(NodeId(2)), Some(NodeKind::Input));
    let i = host.interaction(NodeId(2));
    assert_eq!(i.listeners, 0x1ff);
    assert!(i.focusable);
    assert_eq!(i.role, Role::MultilineTextInput);
    // `setText` leaves the caret at the start, so the insert lands first.
    assert_eq!(ui.inputs.text(2), "!seed");
    assert_eq!(ui.inputs.get(2).unwrap().submit, SubmitKey::ModEnter);

    // Claim sets, the input's and the window list's.
    let set = &ui.host.claims[&2];
    assert_eq!(set.version, 7);
    assert_eq!(
        set.claims,
        [
            Claim::char(Mods::CTRL | Mods::SHIFT, 'k'),
            Claim::named(0, Key::Escape).with(chord_flag::NO_REPEAT),
            Claim::of(claim_kind::PASTE),
        ]
    );
    let window = &ui.host.claims[&craie_ui::mutation::NIL];
    assert_eq!(window.version, 3);
    assert_eq!(
        window.claims,
        [Claim::char(Mods::SHIFT, '?').with(chord_flag::IN_INPUT)]
    );

    // The root owns its layout row.
    let style = host.style(NodeId(0)).to_taffy();
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
    assert_eq!(
        host.children(NodeId(0)),
        [NodeId(3), NodeId(2), NodeId(4), NodeId(6), NodeId(8)]
    );

    // List: templates, items after two splices, a row, an anchor.
    let l = host.lists.get(4).unwrap();
    assert_eq!((l.overscan, l.fallback), (250.0, 36.0));
    assert_eq!(l.templates[0].inset, 16.0);
    assert_eq!(l.templates[1].base, 48.0);
    assert_eq!(l.templates[1].font_size, 0.0);
    assert_eq!(l.len(), 2);
    // Moved (swapped) unchanged, identities kept.
    assert_eq!(l.descs[0].text_len, 70_000);
    assert_eq!((l.descs[0].id, l.descs[0].unchanged), (7, true));
    assert_eq!((l.descs[1].id, l.descs[1].text_len), (5, 42));
    assert_eq!(host.list_index[5], 1);
    assert_eq!(host.lists.policy(0), craie_ui::mutation::Anchor::StickToEnd);

    // Animation: declared transitions (property order; milliseconds
    // arrive as seconds) and two running tweens.
    use craie_ui::animation::{Prop, Timing, Value};
    let tr = &host.transitions[&0];
    assert_eq!(tr.len(), 2);
    assert_eq!((tr[0].prop, tr[1].prop), (Prop::Opacity, Prop::Width));
    let Timing::Curve {
        delay,
        duration,
        x1,
        x2,
        ..
    } = tr[0].timing
    else {
        panic!("{:?}", tr[0].timing)
    };
    assert_eq!((delay, duration, x1, x2), (0.05, 0.25, 0.0, 0.58));
    assert_eq!(
        tr[1].timing,
        Timing::Spring {
            delay: 0.0,
            stiffness: 200.0,
            damping: 20.0,
            mass: 1.0
        }
    );
    let animates: Vec<_> = txn
        .mutations
        .iter()
        .filter_map(|m| match m {
            Mutation::Animate { prop, value, .. } => Some((*prop, *value)),
            _ => None,
        })
        .collect();
    assert_eq!(
        animates,
        [
            (Prop::Fill, Value::Color(0xff00_00ff)),
            (
                Prop::Gap,
                Value::Gap([LengthPercentage::length(4.0), LengthPercentage::length(6.0)])
            ),
            (Prop::Color, Value::Color(0xffff_ffff)),
        ]
    );
    assert!(ui.animating());

    // State styles: the root is a scope with `selected` and custom bit 0;
    // the row's table overlays the selected fill (the narrow variant waits
    // for a window).
    let selected = craie_ui::states::state_bit::SELECTED;
    assert_eq!(ui.state_bits(NodeId(0)), selected | 1);
    assert!(ui.has_variants(NodeId(5)));
    assert_eq!(host.paint[5].fill, 0x2d32_40ff);
    let (mut env, mut variants) = (None, None);
    for m in txn.mutations.iter() {
        match m {
            Mutation::Environment {
                narrow_max,
                compact_max,
            } => env = Some((*narrow_max, *compact_max)),
            Mutation::Variants { id: 5, variants: v } => variants = Some(v.clone()),
            _ => {}
        }
    }
    assert_eq!(env, Some((900.0, 500.0)));
    let v = variants.unwrap();
    assert_eq!((v.len(), v[1].env, v[1].terms[0].mask), (2, 1, 1));
    let narrow = &v[1].values;
    assert_eq!(
        (narrow.border, narrow.radius, narrow.color),
        ((0xff, 1.0), 3.0, None)
    );
    assert_eq!((narrow.opacity, narrow.transform.0[5]), (0.5, 2.0));
    assert_eq!(
        narrow.layout.to_taffy().size.height,
        Dimension::length(44.0)
    );
    assert_eq!(host.colors.get(&0), Some(&0x9aa0_aaff));
    assert!(!host.colors.contains_key(&2));

    // A vector node: the JS-written asset decodes natively.
    assert_eq!(host.kind(NodeId(6)), Some(NodeKind::Vector));
    let asset = host.vectors[&6].asset.as_ref().expect("asset decodes");
    assert_eq!(asset.view_box, [0.0, 0.0, 10.0, 10.0]);
    assert_eq!(asset.items.len(), 1);
    assert_eq!(asset.paints[0], craie_vector::Paint::Solid(0x0080_ffff));
    // A runtime drawing: every shape field survives, and it builds.
    assert_eq!(host.kind(NodeId(8)), Some(NodeKind::Vector));
    let drawing = host.vectors[&8].asset.as_ref().expect("drawing builds");
    assert_eq!(drawing.view_box, [0.0, 0.0, 24.0, 24.0]);
    // The arc strokes (no fill); the polygon fills (no stroke).
    assert_eq!(drawing.items.len(), 2);
    let (arc, tri) = (&drawing.items[0], &drawing.items[1]);
    let dash = arc.dash.as_ref().expect("dashed");
    assert_eq!((dash.array.as_slice(), dash.offset), (&[4.0, 2.0][..], 1.5));
    let craie_vector::asset::ItemStyle::Stroke(line) = arc.style else {
        panic!("{:?}", arc.style)
    };
    assert_eq!(line.width, 2.0);
    assert_eq!(
        (line.join, line.cap),
        (craie_vector::LineJoin::Round, craie_vector::LineCap::Square)
    );
    assert_eq!(drawing.paints[0], craie_vector::Paint::Solid(0x1122_33ff));
    assert_eq!(
        tri.style,
        craie_vector::asset::ItemStyle::Fill(craie_vector::FillRule::EvenOdd)
    );
    assert_eq!(tri.opacity, 0.5);
    assert!(
        (tri.transform.0[4] - 24.0).abs() < 1e-4,
        "{:?}",
        tri.transform
    );

    // WriteClipboard, addressed to no node.
    assert_eq!(ui.inputs.clipboard.get().as_deref(), Some("copied"));
}
