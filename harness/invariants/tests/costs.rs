//! Cost invariants (ARCHITECTURE.md §18): work the architecture promises
//! not to repeat is counted and asserted.
//!
//! - A color change does zero shapes and zero layouts.
//! - A translation does zero layouts and rebuilds no chunk.
//! - An unchanged frame rebuilds zero chunks and uploads zero bytes.
//! - An atlas relocation does zero paragraph layouts.
//! - A transform or opacity tween does zero layouts.

use craie_core::geom::{Affine, Size};
use craie_harness::drain_uploads;
use craie_scene::RasterAtlas;
use craie_ui::host::NodeId;
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;

const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};

fn column() -> taffy::Style {
    taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        },
        ..taffy::Style::default()
    }
}

/// Root column (0) with a box (1) holding a paragraph (2), and a second
/// paragraph (3).
fn ui() -> Ui {
    let mut ui = Ui::new(2.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &column())
        .fill(0, 0x1415_18FF)
        .append(NIL, 0);
    t.create(1, NodeKind::View)
        .fill(1, 0x2A2D_38FF)
        .append(0, 1);
    t.create(2, NodeKind::Text)
        .text(2, "retained native state", 16.0, 0xECEC_F0FF)
        .append(1, 2);
    t.create(3, NodeKind::Text)
        .text(3, "another paragraph", 14.0, 0x9AA0_AEFF)
        .append(0, 3);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    drain_uploads(ui.scene_mut());
    ui
}

fn frame(ui: &mut Ui, t: &Transaction<'_>) -> craie_core::counters::Counters {
    let before = ui.counters();
    ui.apply_txn(t).unwrap();
    ui.render(VIEW);
    ui.counters().since(&before)
}

#[test]
fn color_change_does_no_shapes_and_no_layouts() {
    let mut ui = ui();
    let mut t = Transaction::new(2);
    t.text(2, "retained native state", 16.0, 0xFF00_00FF)
        .fill(1, 0x00FF_00FF);
    let c = frame(&mut ui, &t);
    assert_eq!(c.shapes, 0, "{c:?}");
    assert_eq!(c.layout_passes, 0, "{c:?}");
    assert_eq!(
        c.chunks_built, 0,
        "a color change patches, it does not rebuild: {c:?}"
    );
    assert!(c.paints_patched >= 2, "{c:?}");
    // Only paint records went up.
    let bytes = drain_uploads(ui.scene_mut());
    assert!(bytes > 0 && bytes <= 64, "uploaded {bytes} bytes");
}

#[test]
fn translation_does_no_layouts_and_rebuilds_no_chunk() {
    let mut ui = ui();
    let mut t = Transaction::new(2);
    t.transform(1, Affine::translate(30.0, 10.0));
    let c = frame(&mut ui, &t);
    assert_eq!(c.layout_passes, 0, "{c:?}");
    assert_eq!(c.shapes, 0, "{c:?}");
    assert_eq!(c.chunks_built, 0, "{c:?}");
    // Moving again patches the one record.
    drain_uploads(ui.scene_mut());
    let mut t = Transaction::new(3);
    t.transform(1, Affine::translate(60.0, 10.0));
    let c = frame(&mut ui, &t);
    assert_eq!(
        (c.layout_passes, c.chunks_built, c.draw_orders),
        (0, 0, 0),
        "{c:?}"
    );
    assert_eq!(c.transforms_written, 1, "{c:?}");
}

#[test]
fn scroll_does_no_layouts_and_rebuilds_no_chunk() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let mut s = column();
    s.overflow = taffy::Point {
        x: taffy::Overflow::Scroll,
        y: taffy::Overflow::Scroll,
    };
    s.size.height = taffy::Dimension::length(100.0);
    t.create(0, NodeKind::View).layout(0, &s).append(NIL, 0);
    let mut row = taffy::Style::default();
    row.size.height = taffy::Dimension::length(40.0);
    row.flex_shrink = 0.0;
    for i in 1..=10u32 {
        t.create(i, NodeKind::View)
            .layout(i, &row)
            .fill(i, 0x3344_55FF)
            .append(0, i);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let before = ui.counters();
    ui.scroll_to(NodeId(0), 0.0, 25.0);
    ui.render(VIEW);
    let c = ui.counters().since(&before);
    assert_eq!(
        (c.layout_passes, c.shapes, c.chunks_built),
        (0, 0, 0),
        "{c:?}"
    );
    assert_eq!(c.transforms_written, 1, "{c:?}");
}

#[test]
fn unchanged_frame_rebuilds_nothing_and_uploads_nothing() {
    let mut ui = ui();
    let before = ui.counters();
    assert!(!ui.needs_paint());
    ui.render(VIEW);
    let c = ui.counters().since(&before);
    assert_eq!(c, Default::default(), "{c:?}");
    assert_eq!(drain_uploads(ui.scene_mut()), 0);
}

#[test]
fn tween_steps_do_no_layouts() {
    let mut ui = ui();
    for i in 0..10 {
        let mut t = Transaction::new(10 + i);
        t.transform(1, Affine::rotate(i as f32 * 0.05))
            .opacity(3, 1.0 - i as f32 * 0.05);
        let c = frame(&mut ui, &t);
        assert_eq!((c.layout_passes, c.shapes), (0, 0), "step {i}: {c:?}");
        // Opacity crosses 1.0 once (a layer appears); after that no
        // chunk rebuilds either.
        if i > 1 {
            assert_eq!(c.chunks_built, 0, "step {i}: {c:?}");
        }
    }
}

/// A tiny atlas: paragraph A fills it, scrolls away, and paragraph B's
/// glyphs evict A's. Scrolling A back re-rasterizes its glyphs from the
/// cache keys; no paragraph is shaped again.
#[test]
fn atlas_relocation_does_no_paragraph_layouts() {
    let mut ui = Ui::new(2.0);
    ui.scene_mut().atlas = RasterAtlas::with_budget(128, 1, 1);
    let mut t = Transaction::new(1);
    let mut s = column();
    s.overflow = taffy::Point {
        x: taffy::Overflow::Scroll,
        y: taffy::Overflow::Scroll,
    };
    t.create(0, NodeKind::View).layout(0, &s).append(NIL, 0);
    let mut tall = taffy::Style::default();
    tall.size.height = taffy::Dimension::length(1200.0);
    tall.flex_shrink = 0.0;
    t.create(1, NodeKind::Text)
        .text(1, "abcdefghijklm", 20.0, 0xFFFF_FFFF)
        .append(0, 1);
    t.create(2, NodeKind::View).layout(2, &tall).append(0, 2);
    t.create(3, NodeKind::Text)
        .text(3, "nopqrstuvwxyz", 20.0, 0xFFFF_FFFF)
        .append(0, 3);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let shapes = ui.counters().shapes;
    // B into view (A out): B's glyphs evict A's unpinned rasters.
    ui.scroll_to(NodeId(0), 0.0, 1300.0);
    ui.render(VIEW);
    assert!(
        ui.scene().atlas.stats.evictions > 0,
        "the budget must force evictions"
    );
    // A back into view: its rasters are re-created, its paragraph is not
    // reshaped.
    let rerasters = ui.text.cache.stats.rerasters;
    ui.scroll_to(NodeId(0), 0.0, 0.0);
    ui.render(VIEW);
    assert!(
        ui.text.cache.stats.rerasters > rerasters,
        "evicted glyphs must re-raster"
    );
    assert_eq!(ui.counters().shapes, shapes, "relocation must not reshape");
}

/// An input's color-only config change rebuilds its chunk but runs no
/// layout and shapes nothing.
#[test]
fn input_color_change_does_no_layouts() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(200.0),
        height: taffy::Dimension::length(30.0),
    };
    t.create(0, NodeKind::Input)
        .layout(0, &s)
        .input_config(0, 16.0, 0xFFFF_FFFF, "type", false)
        .append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let mut t = Transaction::new(2);
    t.input_config(0, 16.0, 0xFF00_00FF, "type", false);
    let c = frame(&mut ui, &t);
    assert_eq!((c.layout_passes, c.shapes), (0, 0), "{c:?}");
}

/// A non-empty input: a color-only change patches paint records; the
/// editor's buffer is not reshaped.
#[test]
fn nonempty_input_color_change_does_no_shapes() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(200.0),
        height: taffy::Dimension::length(30.0),
    };
    t.create(0, NodeKind::Input)
        .layout(0, &s)
        .input_config(0, 16.0, 0xFFFF_FFFF, "", false)
        .command(0, craie_ui::mutation::Command::SetText("typed".into()))
        .append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let mut t = Transaction::new(2);
    t.input_config(0, 16.0, 0xFF00_00FF, "", false);
    let c = frame(&mut ui, &t);
    assert_eq!(
        (c.layout_passes, c.shapes, c.chunks_built),
        (0, 0, 0),
        "{c:?}"
    );
    assert!(c.paints_patched > 0, "{c:?}");
}

/// Positive control for the zero-shape color test: typing into an input
/// reshapes its buffer, and the counter sees it (Parley's driver shapes
/// inside the edit, outside `InputState::layout`).
#[test]
fn typing_counts_shapes() {
    use craie_ui::events::{Button, Event, Key, KeyInput, Mods};
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let mut s = taffy::Style::default();
    s.size = taffy::Size {
        width: taffy::Dimension::length(200.0),
        height: taffy::Dimension::length(30.0),
    };
    t.create(0, NodeKind::Input)
        .layout(0, &s)
        .input_config(0, 16.0, 0xFFFF_FFFF, "", false)
        .append(NIL, 0);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    ui.dispatch(&Event::PointerDown {
        x: 5.0,
        y: 5.0,
        button: Button::Primary,
        mods: Mods::default(),
    });
    ui.render(VIEW);
    for (i, ch) in ["a", "b", "c"].into_iter().enumerate() {
        let before = ui.counters();
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Unknown,
            text: Some(ch.into()),
            char: Some(ch.into()),
            mods: Mods::default(),
        }));
        ui.render(VIEW);
        let c = ui.counters().since(&before);
        assert_eq!(c.shapes, 1, "keystroke {i}: {c:?}");
    }

    // Each case: (event, expected reshapes). Composition-only changes
    // keep the committed text but still reshape.
    let key = |key: Key| {
        Event::KeyDown(KeyInput {
            key,
            text: None,
            char: None,
            mods: Mods::default(),
        })
    };
    let preedit = |cursor: usize| Event::ImePreedit {
        text: "かな".into(),
        cursor: Some((cursor, cursor)),
    };
    let cases = [
        ("clean navigation", key(Key::Left), 0),
        ("preedit", preedit(0), 1),
        ("same preedit, caret moved", preedit(3), 1),
        ("same preedit again", preedit(6), 1),
        ("finish composition", Event::ImeDone, 1),
        ("finish with nothing composing", Event::ImeDone, 0),
        ("preedit", preedit(6), 1),
        (
            "empty preedit clears",
            Event::ImePreedit {
                text: String::new(),
                cursor: None,
            },
            1,
        ),
        ("commit", Event::ImeCommit("x".into()), 1),
        ("backspace", key(Key::Backspace), 1),
        ("navigation to start", key(Key::Home), 0),
        ("backspace at start", key(Key::Backspace), 0),
    ];
    // Oracle, independent of the counter: a reshape builds a new layout
    // (shaped data, including its font table) while the old one lives,
    // so that data moves exactly when the editor reshaped (0 versus at
    // least 1). The buffer is never empty here.
    let line_ptr = |ui: &Ui| {
        use craie_ui::text::parley::PositionedLayoutItem;
        let layout = ui.inputs.get(0).unwrap().editor.try_layout().unwrap();
        let line = layout.lines().next().unwrap();
        let Some(PositionedLayoutItem::GlyphRun(run)) = line.items().next() else {
            panic!("no glyph run");
        };
        run.run().font() as *const _ as usize
    };
    for (what, event, want) in cases {
        let before = ui.counters();
        let ptr = line_ptr(&ui);
        ui.dispatch(&event);
        ui.render(VIEW);
        let c = ui.counters().since(&before);
        assert_eq!(c.shapes, want, "{what}: {c:?}");
        assert_eq!(line_ptr(&ui) != ptr, want > 0, "{what}: oracle");
    }
}
