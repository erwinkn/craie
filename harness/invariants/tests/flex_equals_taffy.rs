//! E05: the owned flex engine equals Taffy 0.14 bit for bit, cold and
//! after edits (warm, cached relayouts), on generated trees; and every
//! `LayoutOutput` of direct calls under generated inputs.

use craie_core::rng::Rng;
use craie_harness::flex_oracle::{gen_case, gen_edits};

const SEEDS: u64 = 2400;

#[test]
fn generated_trees_equal_taffy() {
    let mut compared = 0;
    let mut nodes = 0;
    for seed in 1..=SEEDS {
        let mut rng = Rng::new(seed);
        let case = gen_case(&mut rng, 60, 5);
        let edits = gen_edits(&mut rng, &case, 6);
        if let Err(e) = case.compare(&edits) {
            panic!("seed {seed}: {e}\n{case:#?}");
        }
        compared += 1;
        nodes += case.styles.len();
    }
    eprintln!("E05: {compared} trees ({nodes} nodes), each cold and after 6 edits: equal");
    assert_eq!(compared, SEEDS);
}

/// Direct calls: every run mode, sizing mode, requested axis,
/// definiteness, parent size, and available space, each call twice (the
/// second from the cache); full outputs and all layouts compared.
#[test]
fn direct_outputs_equal_taffy() {
    let mut calls = 0;
    for seed in 1..=SEEDS {
        let mut rng = Rng::new(seed ^ 0x6b6b);
        let case = gen_case(&mut rng, 40, 5);
        if let Err(e) = case.compare_outputs(&mut rng, 12) {
            panic!("seed {seed}: {e}\n{case:#?}");
        }
        calls += 12;
    }
    eprintln!("E05: {calls} direct calls (each twice): equal");
}

/// The trees `Gen` builds (Craie's own style mix, as the other oracles
/// see it): each root subtree of each step becomes a case, its leaves
/// measured by kind.
#[test]
fn gen_trees_equal_taffy() {
    use craie_harness::Gen;
    use craie_harness::flex_oracle::{FlexCase, Measure};
    use craie_ui::host::ROOT;
    use craie_ui::mutation::NodeKind;
    use craie_ui::ui::Ui;
    use taffy::{AvailableSpace, Size};

    let mut compared = 0;
    for seed in 1..=40u64 {
        let mut generator = Gen::new(seed);
        let mut ui = Ui::new(2.0);
        ui.apply_txn(&generator.mount()).unwrap();
        for step in 0..=12 {
            if step > 0 {
                let txn = generator.step(&ui);
                ui.apply_txn(&txn).unwrap();
            }
            for root in ui.host.children(ROOT).to_vec() {
                let mut case = FlexCase {
                    styles: Vec::new(),
                    parents: Vec::new(),
                    measures: Vec::new(),
                    available: Size {
                        width: AvailableSpace::Definite(400.0),
                        height: AvailableSpace::Definite(300.0),
                    },
                };
                let mut stack = vec![(root, u32::MAX)];
                while let Some((id, parent)) = stack.pop() {
                    let index = case.styles.len() as u32;
                    case.styles.push(ui.host.style(id).to_taffy());
                    case.parents.push(parent);
                    let seed = id.0;
                    // Text follows its content, so text edits change it.
                    let chars =
                        ui.host
                            .paragraph(id)
                            .map_or(seed as usize, |p| p.text.len()) as u32;
                    case.measures.push(match ui.host.kind(id) {
                        Some(NodeKind::Text) | Some(NodeKind::Input) => Measure::Text {
                            words: 1 + chars / 5 % 30,
                            word: 9.0 + (chars % 7) as f32 * 3.5,
                            line: 18.0,
                        },
                        Some(NodeKind::Vector) => Measure::Aspect(1.0 + (seed % 3) as f32 * 0.5),
                        _ => Measure::Empty,
                    });
                    for child in ui.host.children(id).iter().rev() {
                        stack.push((*child, index));
                    }
                }
                if let Err(e) = case.compare(&[]) {
                    panic!("gen seed {seed} step {step}: {e}\n{case:#?}");
                }
                compared += 1;
            }
        }
    }
    eprintln!("E05: {compared} Gen trees equal");
    assert!(compared >= 40 * 13);
}
