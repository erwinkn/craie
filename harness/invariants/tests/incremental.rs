//! The central invariant: incremental update equals clean rebuild, over
//! seeded mutation sequences. Layout, drawn scene, hit tests, and
//! semantics must match a `Ui` built from the final state in one
//! transaction.

use craie_core::geom::Size;
use craie_harness::{Gen, compare, rebuild, without_animation};
use craie_ui::ui::Ui;

const VIEW: Size = Size {
    width: 480.0,
    height: 360.0,
};

/// Pinned seeds: a failure reproduces exactly.
const SEEDS: [u64; 8] = [1, 2, 3, 5, 8, 13, 0xC0FFEE, 0xDEAD_BEEF];
const STEPS: usize = 60;
/// Layout in logical units and scene in device px: both paths run the
/// same arithmetic, so the tolerance only absorbs float reassociation.
const TOL: f32 = 0.01;

fn run(seed: u64, scale: f32) {
    let mut g = Gen::new(seed);
    let mut ui = Ui::new(scale);
    // The twin gets every transaction without animation: at rest, the
    // animated Ui equals it.
    let mut twin = Ui::new(scale);
    let mount = g.mount();
    ui.apply_txn(&mount).unwrap();
    twin.apply_txn(&mount).unwrap();
    ui.render(VIEW);
    twin.render(VIEW);
    let apply = |ui: &mut Ui, twin: &mut Ui, t: &craie_ui::mutation::Transaction<'static>| {
        let plain = without_animation(t, twin);
        ui.apply_txn(t)
            .unwrap_or_else(|e| panic!("seed {seed}: invalid txn {e:?}\n{:?}", t.mutations));
        twin.apply_txn(&plain).unwrap();
    };
    // The clock: frame-sized steps, with a pause (past the settle time)
    // every few steps, so spaces move, settle, and move again.
    let mut now = 0.0;
    for step in 0..STEPS {
        now += if step % 4 == 3 { 0.25 } else { 0.016 };
        ui.set_time(now);
        twin.set_time(now);
        let t = g.step(&ui);
        apply(&mut ui, &mut twin, &t);
        ui.render(VIEW);
        if let Some(t) = g.select(&mut ui) {
            apply(&mut ui, &mut twin, &t);
        }
        twin.set_text_selection(ui.text_selection());
        if let Some(t) = g.animate(&ui) {
            apply(&mut ui, &mut twin, &t);
        }
        ui.render(VIEW);
        twin.render(VIEW);
        if step % 3 == 0
            && let Some((id, x, y)) = g.scroll(&ui)
        {
            ui.scroll_to(id, x, y);
            twin.scroll_to(id, x, y);
            ui.render(VIEW);
            twin.render(VIEW);
        }
        if step % 5 == 4 {
            // Compare at rest: finish every animation, then settle
            // everything, as a clean build is.
            if let Some(end) = ui.animations_end() {
                now = now.max(end) + 0.001;
                ui.set_time(now);
                ui.render(VIEW);
            }
            assert!(!ui.animating(), "seed {seed} step {step}: animations end");
            now += craie_ui::ui::SETTLE_SECS * 1.5;
            for u in [&mut ui, &mut twin] {
                u.set_time(now);
                u.settle();
                u.render(VIEW);
            }
            assert_eq!(ui.next_settle(), None, "seed {seed} step {step}: at rest");
            let clean = rebuild(&ui, VIEW);
            if let Err(m) = compare(&ui, &clean, VIEW, TOL) {
                panic!("seed {seed} scale {scale} step {step}: {}", m.0);
            }
            // Scroll offsets are native interaction state with a history:
            // a tween that shrank content clamped an offset the twin never
            // clamped (browsers keep such a clamp too). The twin takes the
            // animated Ui's offsets, as a rebuild does; declared state is
            // what must match.
            for i in 0..ui.host.slot_count() {
                let id = craie_ui::host::NodeId(i as u32);
                if ui.host.node(id).is_none() || twin.host.node(id).is_none() {
                    continue;
                }
                let [x, y] = ui.host.spatial[i].scroll;
                if twin.host.spatial[i].scroll != [x, y] {
                    twin.scroll_to(id, x, y);
                }
            }
            twin.render(VIEW);
            // A scroll is motion: rest past it before comparing.
            now += craie_ui::ui::SETTLE_SECS * 1.5;
            for u in [&mut ui, &mut twin] {
                u.set_time(now);
                u.settle();
                u.render(VIEW);
            }
            if let Err(m) = compare(&ui, &twin, VIEW, TOL) {
                panic!(
                    "seed {seed} scale {scale} step {step}: animated vs plain: {}",
                    m.0
                );
            }
        }
    }
}

#[test]
fn incremental_equals_clean_rebuild_1x() {
    for seed in SEEDS {
        run(seed, 1.0);
    }
}

#[test]
fn incremental_equals_clean_rebuild_2x() {
    for seed in SEEDS {
        run(seed, 2.0);
    }
}

/// A viewport change reflows and re-culls; the result must still match
/// a clean build at the new size.
#[test]
fn resize_equals_clean_rebuild() {
    let mut g = Gen::new(77);
    let mut ui = Ui::new(1.0);
    ui.apply_txn(&g.mount()).unwrap();
    ui.render(VIEW);
    for _ in 0..20 {
        let t = g.step(&ui);
        ui.apply_txn(&t).unwrap();
        ui.render(VIEW);
    }
    let small = Size::new(220.0, 500.0);
    ui.invalidate_layout();
    ui.render(small);
    let clean = rebuild(&ui, small);
    compare(&ui, &clean, small, TOL).unwrap_or_else(|m| panic!("{}", m.0));
}

/// Under a tiny atlas budget, every raster of every drawn chunk is
/// resident after each frame: eviction never takes a visible glyph.
#[test]
fn visible_glyphs_stay_resident_under_atlas_pressure() {
    for seed in [4u64, 9, 21] {
        let mut g = Gen::new(seed);
        let mut ui = Ui::new(2.0);
        ui.scene_mut().atlas = craie_scene::RasterAtlas::with_budget(128, 1, 1);
        ui.apply_txn(&g.mount()).unwrap();
        ui.render(VIEW);
        for step in 0..STEPS {
            let t = g.step(&ui);
            ui.apply_txn(&t).unwrap();
            ui.render(VIEW);
            let scene = ui.scene();
            for &id in &scene.draw_list().visible {
                let c = scene.chunk(id).unwrap();
                for glyph in scene.glyphs.get(c.glyphs) {
                    let r = craie_scene::RasterId(glyph.raster);
                    assert!(
                        scene.atlas.entry(r).resident,
                        "seed {seed} step {step}: visible raster {r:?} not resident"
                    );
                }
            }
        }
        assert!(
            ui.scene().atlas.stats.evictions > 0,
            "seed {seed}: no pressure"
        );
    }
}
