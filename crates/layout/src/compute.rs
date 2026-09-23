//! The tree interface of the owned engine, and the parts of layout that
//! are not flexbox: leaves, hidden subtrees, the root call, the cache
//! wrapper, and helpers the flex algorithm shares.
//!
//! Ported from Taffy 0.14 (`compute/mod.rs`, `compute/leaf.rs`,
//! `compute/common/*`, `tree/traits.rs`), MIT license, Copyright (c)
//! 2018 Visly Inc. and the Taffy contributors. The arithmetic is kept
//! in Taffy's order: the differential suite requires bit-equal results.

use taffy::geometry::AbsoluteAxis;
use taffy::util::{MaybeMath, MaybeResolve, ResolveOrZero};
use taffy::{
    AlignContent, AlignContentKeyword, AlignItems, AlignItemsKeyword, AlignmentSafety,
    AvailableSpace, Baselines, BoxSizing, CollapsibleMarginSet, CompactLength, Contain, Dimension,
    Direction, Layout, LayoutInput, LayoutOutput, Line, Overflow, Point, Rect, RequestedAxis,
    RunMode, Size, SizingMode,
};

use crate::LayoutRow;
use crate::flex::FlexScratch;

/// A node in a `LayoutTree`: the host's own id.
pub type Node = u32;

/// Percentages only: Craie builds Taffy without `calc`.
pub(crate) fn no_calc(_: *const (), _: f32) -> f32 {
    0.0
}

/// The host side of the owned engine. The host owns the nodes, their
/// rows, the cache, and the results; `compute_child` is its dispatch
/// (hidden layout, the cache, then flex, a leaf, or a host kind).
pub trait LayoutTree {
    fn row(&self, node: Node) -> &LayoutRow;
    fn child_count(&self, node: Node) -> usize;
    fn child(&self, node: Node, index: usize) -> Node;
    fn compute_child(&mut self, node: Node, inputs: LayoutInput) -> LayoutOutput;
    fn set_layout(&mut self, node: Node, layout: &Layout);
    fn cache_get(&mut self, node: Node, inputs: &LayoutInput) -> Option<LayoutOutput>;
    fn cache_store(&mut self, node: Node, inputs: &LayoutInput, output: LayoutOutput);
    fn cache_clear(&mut self, node: Node);
    /// Buffers the flex algorithm reuses between calls.
    fn scratch(&mut self) -> &mut FlexScratch;
}

pub(crate) fn measure_child_size(
    tree: &mut impl LayoutTree,
    node: Node,
    known_dimensions: Size<Option<f32>>,
    parent_size: Size<Option<f32>>,
    available_space: Size<AvailableSpace>,
    sizing_mode: SizingMode,
    axis: AbsoluteAxis,
) -> f32 {
    tree.compute_child(
        node,
        LayoutInput {
            known_dimensions,
            known_dimensions_are_definite: Size {
                width: true,
                height: true,
            },
            parent_size,
            available_space,
            sizing_mode,
            axis: axis.into(),
            run_mode: RunMode::ComputeSize,
            vertical_margins_are_collapsible: Line::FALSE,
        },
    )
    .size
    .get_abs(axis)
}

pub(crate) fn measure_child_size_both(
    tree: &mut impl LayoutTree,
    node: Node,
    known_dimensions: Size<Option<f32>>,
    parent_size: Size<Option<f32>>,
    available_space: Size<AvailableSpace>,
    sizing_mode: SizingMode,
) -> Size<f32> {
    tree.compute_child(
        node,
        LayoutInput {
            known_dimensions,
            known_dimensions_are_definite: Size {
                width: true,
                height: true,
            },
            parent_size,
            available_space,
            sizing_mode,
            axis: RequestedAxis::Both,
            run_mode: RunMode::ComputeSize,
            vertical_margins_are_collapsible: Line::FALSE,
        },
    )
    .size
}

pub(crate) fn perform_child_layout(
    tree: &mut impl LayoutTree,
    node: Node,
    known_dimensions: Size<Option<f32>>,
    parent_size: Size<Option<f32>>,
    available_space: Size<AvailableSpace>,
    sizing_mode: SizingMode,
) -> LayoutOutput {
    tree.compute_child(
        node,
        LayoutInput {
            known_dimensions,
            known_dimensions_are_definite: Size {
                width: true,
                height: true,
            },
            parent_size,
            available_space,
            sizing_mode,
            axis: RequestedAxis::Both,
            run_mode: RunMode::PerformLayout,
            vertical_margins_are_collapsible: Line::FALSE,
        },
    )
}

/// Lays out `root` into `available_space` (Taffy's `compute_root_layout`
/// without block layout, which Craie does not build).
pub fn compute_root(tree: &mut impl LayoutTree, root: Node, available_space: Size<AvailableSpace>) {
    let output = perform_child_layout(
        tree,
        root,
        Size::NONE,
        available_space.into_options(),
        available_space,
        SizingMode::InherentSize,
    );
    let row = tree.row(root);
    let width = available_space.width.into_option();
    let padding = row.padding().resolve_or_zero(width, no_calc);
    let border = row.border().resolve_or_zero(width, no_calc);
    let margin = row.margin().resolve_or_zero(width, no_calc);
    let overflow = row.overflow();
    let scrollbar_size = Size {
        width: if overflow.y == Overflow::Scroll {
            row.scrollbar_width()
        } else {
            0.0
        },
        height: if overflow.x == Overflow::Scroll {
            row.scrollbar_width()
        } else {
            0.0
        },
    };
    let location = Point {
        x: if row.direction() == Direction::Rtl {
            width.map_or(0.0, |available_width| available_width - output.size.width)
        } else {
            0.0
        },
        y: 0.0,
    };
    tree.set_layout(
        root,
        &Layout {
            order: 0,
            location,
            size: output.size,
            scrollable_overflow_rect: output.scrollable_overflow_rect,
            scrollbar_size,
            padding,
            border,
            margin,
        },
    );
}

/// Returns the cached output for `inputs`, or computes and stores it.
#[inline(always)]
pub fn compute_cached<T: LayoutTree + ?Sized>(
    tree: &mut T,
    node: Node,
    inputs: LayoutInput,
    compute: impl FnOnce(&mut T, Node, LayoutInput) -> LayoutOutput,
) -> LayoutOutput {
    if let Some(output) = tree.cache_get(node, &inputs) {
        return output;
    }
    let output = compute(tree, node, inputs);
    tree.cache_store(node, &inputs, output);
    output
}

/// Zero layout for `node` and its subtree (`display: none`).
pub fn compute_hidden(tree: &mut impl LayoutTree, node: Node) -> LayoutOutput {
    tree.cache_clear(node);
    tree.set_layout(node, &Layout::with_order(0));
    for index in 0..tree.child_count(node) {
        let child = tree.child(node, index);
        tree.compute_child(child, LayoutInput::HIDDEN);
    }
    LayoutOutput::HIDDEN
}

/// A leaf: its styles, and `measure` for its content box.
pub fn compute_leaf(
    inputs: LayoutInput,
    row: &LayoutRow,
    measure: impl FnOnce(Size<Option<f32>>, Size<AvailableSpace>) -> Size<f32>,
) -> LayoutOutput {
    let LayoutInput {
        known_dimensions,
        parent_size,
        available_space,
        sizing_mode,
        run_mode,
        ..
    } = inputs;

    let margin = row.margin().resolve_or_zero(parent_size.width, no_calc);
    let padding = row.padding().resolve_or_zero(parent_size.width, no_calc);
    let border = row.border().resolve_or_zero(parent_size.width, no_calc);
    let padding_border = padding + border;
    let pb_sum = padding_border.sum_axes();
    let box_sizing_adjustment = if row.box_sizing() == BoxSizing::ContentBox {
        pb_sum
    } else {
        Size::ZERO
    };

    let (node_size, node_min_size, node_max_size, aspect_ratio) = match sizing_mode {
        SizingMode::ContentSize => (known_dimensions, Size::NONE, Size::NONE, None),
        SizingMode::InherentSize => {
            let aspect_ratio = row.aspect_ratio();
            let style_size = row
                .size()
                .maybe_resolve(parent_size, no_calc)
                .maybe_apply_aspect_ratio(aspect_ratio)
                .maybe_add(box_sizing_adjustment);
            let style_min_size = row
                .min_size()
                .maybe_resolve(parent_size, no_calc)
                .maybe_apply_aspect_ratio(aspect_ratio)
                .maybe_add(box_sizing_adjustment);
            let style_max_size = row
                .max_size()
                .maybe_resolve(parent_size, no_calc)
                .maybe_add(box_sizing_adjustment);
            (
                known_dimensions.or(style_size),
                style_min_size,
                style_max_size,
                aspect_ratio,
            )
        }
    };

    let overflow = row.overflow();
    let scrollbar_gutter = overflow.transpose().map(|overflow| match overflow {
        Overflow::Scroll => row.scrollbar_width(),
        _ => 0.0,
    });
    // Taffy reserves a leaf's gutter on the right in both directions.
    let mut content_box_inset = padding_border;
    content_box_inset.right += scrollbar_gutter.x;
    content_box_inset.bottom += scrollbar_gutter.y;

    // No box is a block box (Craie builds no block layout), so no leaf
    // collapses its margins through: Taffy's early return always applies.
    if run_mode == RunMode::ComputeSize
        && let Size {
            width: Some(width),
            height: Some(height),
        } = node_size
    {
        let size = Size { width, height }
            .maybe_clamp(node_min_size, node_max_size)
            .maybe_max(padding_border.sum_axes().map(Some));
        return LayoutOutput {
            size,
            scrollable_overflow_rect: Rect::ZERO,
            baselines: Baselines::NONE,
            top_margin: CollapsibleMarginSet::ZERO,
            bottom_margin: CollapsibleMarginSet::ZERO,
            margins_can_collapse_through: false,
        };
    }

    let available_space = Size {
        width: known_dimensions
            .width
            .map(AvailableSpace::from)
            .unwrap_or(available_space.width)
            .maybe_sub(margin.horizontal_axis_sum())
            .maybe_set(known_dimensions.width)
            .maybe_set(node_size.width)
            .map_definite_value(|size| {
                size.maybe_clamp(node_min_size.width, node_max_size.width)
                    - content_box_inset.horizontal_axis_sum()
            }),
        height: known_dimensions
            .height
            .map(AvailableSpace::from)
            .unwrap_or(available_space.height)
            .maybe_sub(margin.vertical_axis_sum())
            .maybe_set(known_dimensions.height)
            .maybe_set(node_size.height)
            .map_definite_value(|size| {
                size.maybe_clamp(node_min_size.height, node_max_size.height)
                    - content_box_inset.vertical_axis_sum()
            }),
    };

    let measured_size = measure(
        match run_mode {
            RunMode::ComputeSize => known_dimensions,
            RunMode::PerformLayout => Size::NONE,
            RunMode::PerformHiddenLayout => unreachable!(),
        },
        available_space,
    );
    let clamped_size = known_dimensions
        .or(node_size)
        .unwrap_or(measured_size + content_box_inset.sum_axes())
        .maybe_clamp(node_min_size, node_max_size);
    let size = Size {
        width: clamped_size.width,
        height: clamped_size.height.max(
            aspect_ratio
                .map(|ratio| clamped_size.width / ratio)
                .unwrap_or(0.0),
        ),
    };
    let size = size.maybe_max(padding_border.sum_axes().map(Some));

    let is_scroll_container = overflow.x.is_scroll_container() || overflow.y.is_scroll_container();
    let is_rtl = row.direction() == Direction::Rtl;
    let start_padding = if is_rtl { padding.right } else { padding.left };
    let end_padding = if is_rtl { padding.left } else { padding.right };
    let scrollable_overflow_rect = Rect {
        left: 0.0,
        right: start_padding
            + measured_size.width
            + if is_scroll_container {
                end_padding
            } else {
                0.0
            },
        top: 0.0,
        bottom: padding.top
            + measured_size.height
            + if is_scroll_container {
                padding.bottom
            } else {
                0.0
            },
    };

    LayoutOutput {
        size,
        scrollable_overflow_rect,
        baselines: Baselines::NONE,
        top_margin: CollapsibleMarginSet::ZERO,
        bottom_margin: CollapsibleMarginSet::ZERO,
        margins_can_collapse_through: false,
    }
}

// Alignment (Taffy's `compute/common/alignment.rs`).

pub(crate) fn resolve_self_alignment_safety(
    alignment: AlignItems,
    overflows: bool,
) -> AlignItemsKeyword {
    if matches!(alignment.safety, AlignmentSafety::Safe) && overflows {
        AlignItemsKeyword::Start
    } else {
        alignment.keyword
    }
}

/// `AlignItems::resolve_self_relative`: `self-start` and `self-end`
/// against the item's own direction.
pub(crate) fn resolve_self_relative(
    value: AlignItems,
    item_direction: Direction,
    container_direction: Direction,
    axis_is_inline: bool,
) -> AlignItems {
    let flip = axis_is_inline && item_direction != container_direction;
    let keyword = match value.keyword {
        AlignItemsKeyword::SelfStart => {
            if flip {
                AlignItemsKeyword::End
            } else {
                AlignItemsKeyword::Start
            }
        }
        AlignItemsKeyword::SelfEnd => {
            if flip {
                AlignItemsKeyword::Start
            } else {
                AlignItemsKeyword::End
            }
        }
        other => other,
    };
    AlignItems {
        keyword,
        safety: value.safety,
    }
}

pub(crate) fn apply_alignment_fallback(
    free_space: f32,
    num_items: usize,
    alignment_mode: AlignContent,
) -> AlignContentKeyword {
    let mut keyword = alignment_mode.keyword;
    let mut is_safe = matches!(alignment_mode.safety, AlignmentSafety::Safe);
    if num_items <= 1 || free_space <= 0.0 {
        (keyword, is_safe) = match keyword {
            AlignContentKeyword::Stretch | AlignContentKeyword::SpaceBetween => {
                (AlignContentKeyword::FlexStart, true)
            }
            AlignContentKeyword::SpaceAround | AlignContentKeyword::SpaceEvenly => {
                (AlignContentKeyword::Center, true)
            }
            other => (other, is_safe),
        };
    }
    if free_space <= 0.0 && is_safe {
        keyword = AlignContentKeyword::Start;
    }
    keyword
}

pub(crate) fn compute_alignment_offset(
    free_space: f32,
    num_items: usize,
    gap: f32,
    alignment_mode: AlignContentKeyword,
    layout_is_flex_reversed: bool,
    is_first: bool,
) -> f32 {
    if is_first {
        match alignment_mode {
            AlignContentKeyword::Start => 0.0,
            AlignContentKeyword::FlexStart => {
                if layout_is_flex_reversed {
                    free_space
                } else {
                    0.0
                }
            }
            AlignContentKeyword::End => free_space,
            AlignContentKeyword::FlexEnd => {
                if layout_is_flex_reversed {
                    0.0
                } else {
                    free_space
                }
            }
            AlignContentKeyword::Center => free_space / 2.0,
            AlignContentKeyword::Stretch => 0.0,
            AlignContentKeyword::SpaceBetween => 0.0,
            AlignContentKeyword::SpaceAround => {
                if free_space >= 0.0 {
                    (free_space / num_items as f32) / 2.0
                } else {
                    free_space / 2.0
                }
            }
            AlignContentKeyword::SpaceEvenly => {
                if free_space >= 0.0 {
                    free_space / (num_items + 1) as f32
                } else {
                    free_space / 2.0
                }
            }
        }
    } else {
        let free_space = free_space.max(0.0);
        gap + match alignment_mode {
            AlignContentKeyword::Start
            | AlignContentKeyword::FlexStart
            | AlignContentKeyword::End
            | AlignContentKeyword::FlexEnd
            | AlignContentKeyword::Center
            | AlignContentKeyword::Stretch => 0.0,
            AlignContentKeyword::SpaceBetween => free_space / (num_items - 1) as f32,
            AlignContentKeyword::SpaceAround => free_space / num_items as f32,
            AlignContentKeyword::SpaceEvenly => free_space / (num_items + 1) as f32,
        }
    }
}

// Scrollable overflow (Taffy's `compute/common/scrollable_overflow.rs`).

/// The rectangle a child adds to its parent's scrollable overflow.
/// `location` is the child's border box from the parent's scroll origin.
pub(crate) fn compute_scrollable_overflow_contribution(
    location: Point<f32>,
    size: Size<f32>,
    scrollable_overflow_rect: Rect<f32>,
    overflow: Point<Overflow>,
    contain: Contain,
    parent_is_scroll_container: bool,
) -> Rect<f32> {
    let is_scroll_container = overflow.x.is_scroll_container() || overflow.y.is_scroll_container();
    let overflow_is_contained = contain.contains_scrollable_overflow();
    let propagates = Point {
        x: !is_scroll_container && !overflow_is_contained && overflow.x == Overflow::Visible,
        y: !is_scroll_container && !overflow_is_contained && overflow.y == Overflow::Visible,
    };
    let end_extent = Size {
        width: if propagates.x {
            size.width.max(scrollable_overflow_rect.right)
        } else {
            size.width
        },
        height: if propagates.y {
            size.height.max(scrollable_overflow_rect.bottom)
        } else {
            size.height
        },
    };
    if end_extent.width <= 0.0 || end_extent.height <= 0.0 {
        return Rect::ZERO;
    }
    let start_extent = Point {
        x: if propagates.x {
            0.0f32.min(scrollable_overflow_rect.left)
        } else {
            0.0
        },
        y: if propagates.y {
            0.0f32.min(scrollable_overflow_rect.top)
        } else {
            0.0
        },
    };
    let contribution = Rect {
        left: location.x + start_extent.x,
        right: location.x + end_extent.width,
        top: location.y + start_extent.y,
        bottom: location.y + end_extent.height,
    };
    let is_wholly_unreachable = contribution.right <= 0.0 || contribution.bottom <= 0.0;
    if parent_is_scroll_container && is_wholly_unreachable {
        Rect::ZERO
    } else {
        contribution
    }
}

// Sizing keywords (Taffy's `compute/common/sizing_keyword.rs`).

pub(crate) enum SizingKeywordResolution {
    /// Measure the item under this available space.
    Measure(AvailableSpace),
    /// The size, without a measure.
    Exact(f32),
}

pub(crate) fn resolve_sizing_keyword(
    style: Dimension,
    stretch_size: Option<f32>,
    percent_resolution_basis: Option<f32>,
) -> Option<SizingKeywordResolution> {
    match style.tag() {
        CompactLength::MIN_CONTENT_TAG => {
            Some(SizingKeywordResolution::Measure(AvailableSpace::MinContent))
        }
        CompactLength::MAX_CONTENT_TAG => {
            Some(SizingKeywordResolution::Measure(AvailableSpace::MaxContent))
        }
        CompactLength::FIT_CONTENT_PX_TAG => Some(SizingKeywordResolution::Measure(
            AvailableSpace::Definite(style.value()),
        )),
        CompactLength::FIT_CONTENT_PERCENT_TAG => percent_resolution_basis.map(|basis| {
            SizingKeywordResolution::Measure(AvailableSpace::Definite(basis * style.value()))
        }),
        CompactLength::FIT_CONTENT_KEYWORD_TAG => stretch_size
            .map(|size| SizingKeywordResolution::Measure(AvailableSpace::Definite(size))),
        CompactLength::STRETCH_TAG => stretch_size.map(SizingKeywordResolution::Exact),
        _ => None,
    }
}

/// Resolves sizing keywords on an absolutely positioned item's size
/// into `known_dimensions`. `area_size` is its containing block.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_absolute_sizing_keywords(
    tree: &mut impl LayoutTree,
    node: Node,
    known_dimensions: &mut Size<Option<f32>>,
    size_style: Size<Dimension>,
    area_size: Size<f32>,
    inset: Rect<Option<f32>>,
    margin: Rect<Option<f32>>,
    sizing_mode: SizingMode,
) {
    let stretch_size = Size {
        width: (area_size.width
            - inset.left.unwrap_or(0.0)
            - inset.right.unwrap_or(0.0)
            - margin.left.unwrap_or(0.0)
            - margin.right.unwrap_or(0.0))
        .max(0.0),
        height: (area_size.height
            - inset.top.unwrap_or(0.0)
            - inset.bottom.unwrap_or(0.0)
            - margin.top.unwrap_or(0.0)
            - margin.bottom.unwrap_or(0.0))
        .max(0.0),
    };
    let keyword_width = if known_dimensions.width.is_none() {
        resolve_sizing_keyword(
            size_style.width,
            Some(stretch_size.width),
            Some(area_size.width),
        )
    } else {
        None
    };
    let keyword_height = if known_dimensions.height.is_none() {
        resolve_sizing_keyword(
            size_style.height,
            Some(stretch_size.height),
            Some(area_size.height),
        )
    } else {
        None
    };
    match (keyword_width, keyword_height) {
        (
            Some(SizingKeywordResolution::Measure(available_width)),
            Some(SizingKeywordResolution::Measure(available_height)),
        ) => {
            let measured_size = measure_child_size_both(
                tree,
                node,
                Size::NONE,
                area_size.map(Some),
                Size {
                    width: available_width,
                    height: available_height,
                },
                sizing_mode,
            );
            *known_dimensions = measured_size.map(Some);
        }
        (keyword_width, keyword_height) => {
            if let Some(resolution) = keyword_width {
                known_dimensions.width = Some(match resolution {
                    SizingKeywordResolution::Exact(width) => width,
                    SizingKeywordResolution::Measure(available_width) => measure_child_size(
                        tree,
                        node,
                        *known_dimensions,
                        area_size.map(Some),
                        Size {
                            width: available_width,
                            height: AvailableSpace::Definite(stretch_size.height),
                        },
                        sizing_mode,
                        AbsoluteAxis::Horizontal,
                    ),
                });
            }
            if let Some(resolution) = keyword_height {
                known_dimensions.height = Some(match resolution {
                    SizingKeywordResolution::Exact(height) => height,
                    SizingKeywordResolution::Measure(available_height) => measure_child_size(
                        tree,
                        node,
                        *known_dimensions,
                        area_size.map(Some),
                        Size {
                            width: known_dimensions
                                .width
                                .map(AvailableSpace::Definite)
                                .unwrap_or(AvailableSpace::Definite(stretch_size.width)),
                            height: available_height,
                        },
                        sizing_mode,
                        AbsoluteAxis::Vertical,
                    ),
                });
            }
        }
    }
}
