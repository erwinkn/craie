use std::collections::HashMap;

use craie_core::geom::{Rect, Size};
use craie_harness::Gen;
use craie_ui::host::{Host, NodeId, ROOT};
use craie_ui::layout::LayoutData;
use craie_ui::ui::Ui;
use taffy::{AvailableSpace, Layout, TaffyTree};

const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};

#[test]
fn layout_row_matches_reference_taffy_style() {
    for seed in [3, 11, 29, 71] {
        let mut generator = Gen::new(seed);
        let mut ui = Ui::new(2.0);
        ui.apply_txn(&generator.mount()).unwrap();
        ui.render(VIEW);
        assert_reference(&ui, seed, 0);
        for step in 1..=5 {
            let txn = generator.step(&ui);
            ui.apply_txn(&txn).unwrap();
            ui.render(VIEW);
            assert_reference(&ui, seed, step);
        }
    }
}

fn assert_reference(ui: &Ui, seed: u64, step: usize) {
    let host = &ui.host;
    let mut tree: TaffyTree<taffy::Size<f32>> = TaffyTree::new();
    tree.disable_rounding();
    let mut map = HashMap::new();
    for root in host.children(ROOT) {
        build_node(host, &ui.layouts, &mut tree, &mut map, *root);
    }
    for root in host.children(ROOT) {
        let taffy_root = map[root];
        tree.compute_layout_with_measure(
            taffy_root,
            taffy::Size {
                width: AvailableSpace::Definite(VIEW.width),
                height: AvailableSpace::Definite(VIEW.height),
            },
            |inputs, _, measured, style| {
                taffy::compute_leaf_layout(
                    inputs,
                    style,
                    |_, _| 0.0,
                    |known, _| {
                        let measured = measured.as_deref().copied().unwrap_or_default();
                        taffy::Size {
                            width: known.width.unwrap_or(measured.width),
                            height: known.height.unwrap_or(measured.height),
                        }
                    },
                )
            },
        )
        .unwrap();
        compare_subtree(host, &ui.layouts, &tree, &map, *root, seed, step);
    }
}

fn build_node(
    host: &Host,
    layouts: &craie_ui::layout::Layouts,
    tree: &mut TaffyTree<taffy::Size<f32>>,
    map: &mut HashMap<NodeId, taffy::NodeId>,
    id: NodeId,
) -> taffy::NodeId {
    let style = host.style(id).to_taffy();
    let children: Vec<_> = host
        .children(id)
        .iter()
        .map(|child| build_node(host, layouts, tree, map, *child))
        .collect();
    let node = if children.is_empty() {
        let data = layouts.data(id);
        tree.new_leaf_with_context(
            style,
            taffy::Size {
                width: (data.rect.size.width - data.insets[0]).max(0.0),
                height: (data.rect.size.height - data.insets[1]).max(0.0),
            },
        )
    } else {
        tree.new_with_children(style, &children)
    }
    .unwrap();
    map.insert(id, node);
    node
}

fn compare_subtree(
    host: &Host,
    layouts: &craie_ui::layout::Layouts,
    tree: &TaffyTree<taffy::Size<f32>>,
    map: &HashMap<NodeId, taffy::NodeId>,
    id: NodeId,
    seed: u64,
    step: usize,
) {
    let reference = tree.layout(map[&id]).unwrap();
    let got = layouts.data(id);
    let want = data_from_taffy(reference);
    assert_data_eq(got, want, id, seed, step);
    for child in host.children(id) {
        compare_subtree(host, layouts, tree, map, *child, seed, step);
    }
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

fn assert_data_eq(got: LayoutData, want: LayoutData, id: NodeId, seed: u64, step: usize) {
    assert_rect_eq(got.rect, want.rect, "rect", id, seed, step);
    assert_pair_eq(got.content, want.content, "content", id, seed, step);
    assert_pair_eq(got.insets, want.insets, "insets", id, seed, step);
    assert_rect_eq(got.clip_box, want.clip_box, "clip", id, seed, step);
    assert_pair_eq(
        got.scroll_extent,
        want.scroll_extent,
        "scroll_extent",
        id,
        seed,
        step,
    );
}

fn assert_rect_eq(got: Rect, want: Rect, label: &str, id: NodeId, seed: u64, step: usize) {
    assert_bits(got.origin.x, want.origin.x, label, id, seed, step);
    assert_bits(got.origin.y, want.origin.y, label, id, seed, step);
    assert_bits(got.size.width, want.size.width, label, id, seed, step);
    assert_bits(got.size.height, want.size.height, label, id, seed, step);
}

fn assert_pair_eq(got: [f32; 2], want: [f32; 2], label: &str, id: NodeId, seed: u64, step: usize) {
    assert_bits(got[0], want[0], label, id, seed, step);
    assert_bits(got[1], want[1], label, id, seed, step);
}

fn assert_bits(got: f32, want: f32, label: &str, id: NodeId, seed: u64, step: usize) {
    assert_eq!(
        got.to_bits(),
        want.to_bits(),
        "{label}: node {} seed {seed} step {step}: got {got:?}, want {want:?}",
        id.0
    );
}
