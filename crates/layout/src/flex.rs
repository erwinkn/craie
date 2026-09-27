//! The owned flexbox algorithm over `LayoutRow`.
//!
//! Ported from Taffy 0.14 `compute/flexbox.rs` (MIT license, Copyright
//! (c) 2018 Visly Inc. and the Taffy contributors), without
//! `flexbox_balance`, which Craie does not build. The steps, their order,
//! and the float arithmetic follow Taffy, so results are bit-equal; the
//! differential suite (`tests/flex_equals_taffy.rs`) holds that.
//!
//! Differences in structure: the algorithm reads rows directly, flex
//! lines are index ranges into one item buffer, and the item and line
//! buffers come from `FlexScratch`, so a warm layout does not allocate.

use taffy::style_helpers::{TaffyMaxContent, TaffyMinContent};
use taffy::util::{MaybeMath, MaybeResolve, ResolveOrZero};
use taffy::{
    AlignContent, AlignContentKeyword, AlignItems, AlignItemsKeyword, AlignSelf, AvailableSpace,
    Baselines, BoxGenerationMode, BoxSizing, Contain, CoreStyle, Dimension, Direction,
    FlexDirection, JustifyContent, Layout, LayoutInput, LayoutOutput, Line, Overflow, Point,
    Position, Rect, RequestedAxis, RunMode, Size, SizingMode,
};

use crate::axis::{Dir, PointDir, RectDir, RectSum, SizeDir, is_multi_line, is_wrap_reverse};
use crate::compute::{
    LayoutTree, Node, SizingKeywordResolution, apply_alignment_fallback, compute_alignment_offset,
    compute_scrollable_overflow_contribution, measure_child_size, measure_child_size_both, no_calc,
    perform_child_layout, resolve_absolute_sizing_keywords, resolve_self_alignment_safety,
    resolve_self_relative, resolve_sizing_keyword,
};

/// Item and line buffers, reused across flex calls. Nested containers
/// each take one pair, so the pool holds one pair per level of nesting.
/// A call takes the smallest buffers that hold its children (best fit:
/// if the ancestor chain fit before, it fits again), and lines reserve
/// one per item. So a relayout of containers already laid out allocates
/// nothing. A buffer that grew past `KEEP_ITEMS` is dropped, not kept:
/// one wide container must not hold its peak memory (248 bytes per
/// item) for the life of the tree. Wider containers allocate per call,
/// as Taffy does.
#[derive(Default)]
pub struct FlexScratch {
    items: Vec<Vec<FlexItem>>,
    lines: Vec<Vec<FlexLine>>,
}

/// The intermediate results of the algorithm for one item.
struct FlexItem {
    node: Node,
    /// The index among all of the container's children.
    order: u32,
    size: Size<Option<f32>>,
    size_style: Size<Dimension>,
    min_size: Size<Option<f32>>,
    max_size: Size<Option<f32>>,
    aspect_ratio: Option<f32>,
    align_self: AlignSelf,
    overflow: Point<Overflow>,
    contain: Contain,
    scrollbar_width: f32,
    flex_shrink: f32,
    flex_grow: f32,
    /// The used flex basis is definite (not from content).
    flex_basis_is_definite: bool,
    /// The minimum main size, content-based automatic minimum included.
    resolved_minimum_main_size: f32,
    inset: Rect<Option<f32>>,
    margin: Rect<f32>,
    margin_is_auto: Rect<bool>,
    padding: Rect<f32>,
    border: Rect<f32>,
    flex_basis: f32,
    inner_flex_basis: f32,
    violation: f32,
    frozen: bool,
    /// The min- or max-content flex fraction.
    content_flex_fraction: f32,
    hypothetical_inner_size: Size<f32>,
    hypothetical_outer_size: Size<f32>,
    target_size: Size<f32>,
    outer_target_size: Size<f32>,
    baseline: f32,
    /// Offsets from the natural flow position (alignment, justification,
    /// relative position); margin, padding, and border excluded.
    offset_main: f32,
    offset_cross: f32,
}

impl FlexItem {
    fn is_scroll_container(&self) -> bool {
        self.overflow.x.is_scroll_container() | self.overflow.y.is_scroll_container()
    }

    /// `align-self: baseline` and neither cross margin `auto`.
    fn participates_in_baseline_alignment(&self, dir: FlexDirection) -> bool {
        self.align_self == AlignSelf::BASELINE
            && !self.margin_is_auto.cross_start(dir)
            && !self.margin_is_auto.cross_end(dir)
    }
}

/// A flex line: the items `start..end` of the item buffer.
#[derive(Clone, Copy)]
struct FlexLine {
    start: usize,
    end: usize,
    cross_size: f32,
    offset_cross: f32,
}

impl FlexLine {
    fn new(start: usize, end: usize) -> FlexLine {
        FlexLine {
            start,
            end,
            cross_size: 0.0,
            offset_cross: 0.0,
        }
    }

    fn len(&self) -> usize {
        self.end - self.start
    }
}

struct AlgoConstants {
    dir: FlexDirection,
    layout_direction: Direction,
    is_row: bool,
    is_column: bool,
    is_wrap: bool,
    is_wrap_reverse: bool,
    min_size: Size<Option<f32>>,
    max_size: Size<Option<f32>>,
    margin: Rect<f32>,
    border: Rect<f32>,
    /// Padding, border, and scrollbar gutter.
    content_box_inset: Rect<f32>,
    scrollbar_gutter: Point<f32>,
    is_scroll_container: bool,
    gap: Size<f32>,
    align_items: AlignItems,
    align_content: AlignContent,
    justify_content: Option<JustifyContent>,
    node_outer_size: Size<Option<f32>>,
    node_inner_size: Size<Option<f32>>,
    /// The known main size is definite (not derived from content).
    known_main_size_is_definite: bool,
    has_definite_main_size: bool,
    has_definite_cross_size: bool,
    cross_axis_available_space_is_definite: bool,
    container_size: Size<f32>,
    inner_container_size: Size<f32>,
}

/// Lays out a flex container.
pub fn compute_flex(tree: &mut impl LayoutTree, node: Node, inputs: LayoutInput) -> LayoutOutput {
    let LayoutInput {
        known_dimensions,
        parent_size,
        run_mode,
        ..
    } = inputs;
    let style = tree.row(node);

    let contain = style.contain();
    let aspect_ratio = style.aspect_ratio();
    let padding = style.padding().resolve_or_zero(parent_size.width, no_calc);
    let border = style.border().resolve_or_zero(parent_size.width, no_calc);
    let padding_border_sum = padding.sum_axes() + border.sum_axes();
    let box_sizing_adjustment = if style.box_sizing() == BoxSizing::ContentBox {
        padding_border_sum
    } else {
        Size::ZERO
    };

    let min_size = style
        .min_size()
        .maybe_resolve(parent_size, no_calc)
        .maybe_apply_aspect_ratio(aspect_ratio)
        .maybe_add(box_sizing_adjustment);
    let max_size = style
        .max_size()
        .maybe_resolve(parent_size, no_calc)
        .maybe_apply_aspect_ratio(aspect_ratio)
        .maybe_add(box_sizing_adjustment);
    let clamped_style_size = if inputs.sizing_mode == SizingMode::InherentSize {
        style
            .size()
            .maybe_resolve(parent_size, no_calc)
            .maybe_apply_aspect_ratio(aspect_ratio)
            .maybe_add(box_sizing_adjustment)
            .maybe_clamp(min_size, max_size)
    } else {
        Size::NONE
    };

    // Min and max set with max <= min: min is the size.
    let min_max_definite_size = min_size.zip_map(max_size, |min, max| match (min, max) {
        (Some(min), Some(max)) if max <= min => Some(min),
        _ => None,
    });

    // Padding and border floor the container size.
    let styled_based_known_dimensions = known_dimensions.or(min_max_definite_size
        .or(clamped_style_size)
        .maybe_max(padding_border_sum));

    if run_mode == RunMode::ComputeSize {
        if let Size {
            width: Some(width),
            height: Some(height),
        } = styled_based_known_dimensions
        {
            return LayoutOutput::from_outer_size(Size { width, height });
        }
        if inputs.axis == RequestedAxis::Horizontal
            && let Some(width) = styled_based_known_dimensions.width
        {
            return LayoutOutput::from_outer_size(Size { width, height: 0.0 });
        }
    }

    // Definiteness flags apply only to dimensions the parent passed; the
    // node's own style sizes are always definite.
    let known_dimensions_are_definite = inputs
        .known_dimensions_are_definite
        .zip_map(known_dimensions, |is_definite, known| {
            is_definite || known.is_none()
        });

    let mut output = compute_preliminary(
        tree,
        node,
        LayoutInput {
            known_dimensions: styled_based_known_dimensions,
            known_dimensions_are_definite,
            ..inputs
        },
    );

    // Layout containment suppresses the baseline.
    if contain.suppresses_baseline() {
        output.baselines = Baselines::NONE;
    }
    output
}

fn compute_preliminary(
    tree: &mut impl LayoutTree,
    node: Node,
    inputs: LayoutInput,
) -> LayoutOutput {
    let LayoutInput {
        known_dimensions,
        parent_size,
        available_space,
        run_mode,
        ..
    } = inputs;

    let mut constants = compute_constants(
        tree.row(node),
        known_dimensions,
        inputs.known_dimensions_are_definite,
        parent_size,
        available_space,
    );
    let dir = constants.dir;

    let children = tree.child_count(node);
    let scratch = tree.scratch();
    let mut items = take_best_fit(&mut scratch.items, children);
    let mut lines = take_best_fit(&mut scratch.lines, children.max(1));
    // Exact: amortized growth could pass `KEEP_ITEMS` and the pool
    // would drop the buffer.
    items.reserve_exact(children);
    lines.reserve_exact(children.max(1));

    // 9.1: items.
    generate_anonymous_flex_items(tree, node, &constants, &mut items);

    // 9.2: available space, then flex base sizes.
    let available_space = determine_available_space(known_dimensions, available_space, &constants);
    determine_flex_base_size(tree, &constants, available_space, &mut items);

    // 9.3: lines, then the container's main size.
    collect_flex_lines(&constants, available_space, &items, &mut lines);
    if let Some(inner_main_size) = constants.node_inner_size.main(dir) {
        let outer_main_size = inner_main_size + constants.content_box_inset.main_axis_sum(dir);
        constants
            .inner_container_size
            .set_main(dir, inner_main_size);
        constants.container_size.set_main(dir, outer_main_size);
    } else {
        determine_container_main_size(tree, available_space, &mut items, &lines, &mut constants);
        constants
            .node_inner_size
            .set_main(dir, Some(constants.inner_container_size.main(dir)));
        constants
            .node_outer_size
            .set_main(dir, Some(constants.container_size.main(dir)));
        // Percentage gaps resolve again against the new size.
        let inner_container_size = constants.inner_container_size.main(dir);
        let new_gap = tree
            .row(node)
            .gap()
            .main(dir)
            .maybe_resolve(inner_container_size, no_calc)
            .unwrap_or(0.0);
        constants.gap.set_main(dir, new_gap);
    }

    // 6: flexible lengths.
    for line in lines.iter() {
        resolve_flexible_lengths(&mut items[line.start..line.end], &constants);
    }

    // 9.4: cross sizes.
    for line in lines.iter() {
        determine_hypothetical_cross_size(
            tree,
            &mut items[line.start..line.end],
            &constants,
            available_space,
        );
    }
    calculate_children_base_lines(
        tree,
        known_dimensions,
        available_space,
        &mut items,
        &lines,
        &constants,
    );
    calculate_cross_size(&items, &mut lines, known_dimensions, &constants);
    handle_align_content_stretch(&mut lines, known_dimensions, &constants);
    // Step 10 (visibility: collapse) is not supported, as in Taffy.
    determine_used_cross_size(tree, &mut items, &lines, &constants);

    // 9.5: main-axis alignment.
    distribute_remaining_free_space(&mut items, &lines, &constants);

    // 9.6: cross-axis alignment.
    resolve_cross_axis_auto_margins(&mut items, &lines, &constants);
    let total_line_cross_size =
        determine_container_cross_size(&lines, known_dimensions, &mut constants);

    if run_mode == RunMode::ComputeSize {
        let output = LayoutOutput::from_outer_size(constants.container_size);
        give_back(tree, items, lines);
        return output;
    }

    align_flex_lines_per_align_content(&mut lines, &constants, total_line_cross_size);
    let inflow_overflow_rect = final_layout_pass(tree, &mut items, &lines, &constants);
    let absolute_overflow_rect =
        perform_absolute_layout_on_absolute_children(tree, node, &constants);

    // Hidden children.
    let len = tree.child_count(node);
    for order in 0..len {
        let child = tree.child(node, order);
        if tree.row(child).box_generation_mode() == BoxGenerationMode::None {
            tree.set_layout(child, &Layout::with_order(order as u32));
            perform_child_layout(
                tree,
                child,
                Size::NONE,
                Size::NONE,
                Size::MAX_CONTENT,
                SizingMode::ContentSize,
            );
        }
    }

    // The first baseline comes from the startmost line (the last one
    // under wrap-reverse).
    let first_line = if constants.is_wrap_reverse {
        lines.last()
    } else {
        lines.first()
    };
    let first_vertical_baseline = first_line.and_then(|line| {
        let line_items = &items[line.start..line.end];
        if constants.is_column {
            let item = if dir.is_reverse() {
                line_items.last()
            } else {
                line_items.first()
            };
            item.map(|child| child.baseline)
        } else {
            line_items
                .iter()
                .find(|item| item.participates_in_baseline_alignment(dir))
                .or_else(|| line_items.iter().next())
                .map(|child| child.baseline)
        }
    });

    let output = LayoutOutput::from_sizes_and_baselines(
        constants.container_size,
        inflow_overflow_rect.union(absolute_overflow_rect),
        Baselines::from_first(first_vertical_baseline),
    );
    give_back(tree, items, lines);
    output
}

/// The largest buffer the pool keeps, in items.
const KEEP_ITEMS: usize = 256;

/// The pool's smallest buffer of capacity `need` or more, else its
/// largest (to grow), else a new one.
fn take_best_fit<T>(pool: &mut Vec<Vec<T>>, need: usize) -> Vec<T> {
    let mut best: Option<usize> = None;
    for (i, buffer) in pool.iter().enumerate() {
        let capacity = buffer.capacity();
        best = match best {
            None => Some(i),
            Some(b) => {
                let current = pool[b].capacity();
                let better = if current >= need {
                    capacity >= need && capacity < current
                } else {
                    capacity > current
                };
                Some(if better { i } else { b })
            }
        };
    }
    match best {
        Some(i) => pool.swap_remove(i),
        None => Vec::new(),
    }
}

fn give_back(tree: &mut impl LayoutTree, mut items: Vec<FlexItem>, mut lines: Vec<FlexLine>) {
    items.clear();
    lines.clear();
    if items.capacity() > KEEP_ITEMS {
        items = Vec::new();
    }
    if lines.capacity() > KEEP_ITEMS {
        lines = Vec::new();
    }
    let scratch = tree.scratch();
    scratch.items.push(items);
    scratch.lines.push(lines);
}

fn compute_constants(
    style: &crate::LayoutRow,
    known_dimensions: Size<Option<f32>>,
    known_dimensions_are_definite: Size<bool>,
    parent_size: Size<Option<f32>>,
    available_space: Size<AvailableSpace>,
) -> AlgoConstants {
    let dir = style.flex_direction();
    let is_row = dir.is_row();
    let is_column = dir.is_column();
    let flex_wrap = style.flex_wrap();
    let is_wrap = is_multi_line(flex_wrap);
    let is_wrap_reverse = is_wrap_reverse(flex_wrap);

    let aspect_ratio = style.aspect_ratio();
    let margin = style.margin().resolve_or_zero(parent_size.width, no_calc);
    let padding = style.padding().resolve_or_zero(parent_size.width, no_calc);
    let border = style.border().resolve_or_zero(parent_size.width, no_calc);
    let padding_border_sum = padding.sum_axes() + border.sum_axes();
    let box_sizing_adjustment = if style.box_sizing() == BoxSizing::ContentBox {
        padding_border_sum
    } else {
        Size::ZERO
    };

    let align_items = style.align_items().unwrap_or(AlignItems::STRETCH);
    let align_content = style.align_content().unwrap_or(AlignContent::STRETCH);
    let justify_content = style.justify_content();
    let layout_direction = style.direction();

    // A vertical scroller reserves horizontal space, so the axes swap.
    let overflow = style.overflow();
    let scrollbar_gutter = overflow.transpose().map(|overflow| match overflow {
        Overflow::Scroll => style.scrollbar_width(),
        _ => 0.0,
    });
    let is_scroll_container = overflow.x.is_scroll_container() || overflow.y.is_scroll_container();
    let mut content_box_inset = padding + border;
    content_box_inset.bottom += scrollbar_gutter.y;
    match layout_direction {
        Direction::Ltr => content_box_inset.right += scrollbar_gutter.x,
        Direction::Rtl => content_box_inset.left += scrollbar_gutter.x,
    };

    let node_outer_size = known_dimensions;
    let node_inner_size = node_outer_size.maybe_sub(content_box_inset.sum_axes());
    let known_main_size_is_definite = known_dimensions_are_definite.main(dir);
    let has_definite_main_size =
        known_main_size_is_definite && known_dimensions.main(dir).is_some();
    let has_definite_cross_size =
        known_dimensions_are_definite.cross(dir) && known_dimensions.cross(dir).is_some();
    let cross_axis_available_space_is_definite = has_definite_cross_size
        || matches!(available_space.cross(dir), AvailableSpace::Definite(_));
    let gap = style
        .gap()
        .resolve_or_zero(node_inner_size.or(Size::zero()), no_calc);

    AlgoConstants {
        dir,
        layout_direction,
        is_row,
        is_column,
        is_wrap,
        is_wrap_reverse,
        min_size: style
            .min_size()
            .maybe_resolve(parent_size, no_calc)
            .maybe_apply_aspect_ratio(aspect_ratio)
            .maybe_add(box_sizing_adjustment),
        max_size: style
            .max_size()
            .maybe_resolve(parent_size, no_calc)
            .maybe_apply_aspect_ratio(aspect_ratio)
            .maybe_add(box_sizing_adjustment),
        margin,
        border,
        gap,
        content_box_inset,
        scrollbar_gutter,
        is_scroll_container,
        align_items,
        align_content,
        justify_content,
        node_outer_size,
        node_inner_size,
        known_main_size_is_definite,
        has_definite_main_size,
        has_definite_cross_size,
        cross_axis_available_space_is_definite,
        container_size: Size::zero(),
        inner_container_size: Size::zero(),
    }
}

/// 9.1: every in-flow, displayed child is an item.
fn generate_anonymous_flex_items(
    tree: &impl LayoutTree,
    node: Node,
    constants: &AlgoConstants,
    items: &mut Vec<FlexItem>,
) {
    // Percentage sizes resolve against a definite inner size only.
    let percent_resolution_size = if constants.known_main_size_is_definite {
        constants.node_inner_size
    } else {
        constants.node_inner_size.with_main(constants.dir, None)
    };
    let inner_width = constants.node_inner_size.width;
    for index in 0..tree.child_count(node) {
        let child = tree.child(node, index);
        let child_style = tree.row(child);
        if child_style.position() == Position::Absolute
            || child_style.box_generation_mode() == BoxGenerationMode::None
        {
            continue;
        }
        let aspect_ratio = child_style.aspect_ratio();
        let padding = child_style.padding().resolve_or_zero(inner_width, no_calc);
        let border = child_style.border().resolve_or_zero(inner_width, no_calc);
        let pb_sum = (padding + border).sum_axes();
        let box_sizing_adjustment = if child_style.box_sizing() == BoxSizing::ContentBox {
            pb_sum
        } else {
            Size::ZERO
        };
        let inset = child_style.inset();
        let inner = constants.node_inner_size;
        items.push(FlexItem {
            node: child,
            order: index as u32,
            size: child_style
                .size()
                .maybe_resolve(percent_resolution_size, no_calc)
                .maybe_apply_aspect_ratio(aspect_ratio)
                .maybe_add(box_sizing_adjustment),
            size_style: child_style.size(),
            min_size: child_style
                .min_size()
                .maybe_resolve(percent_resolution_size, no_calc)
                .maybe_add(box_sizing_adjustment),
            max_size: child_style
                .max_size()
                .maybe_resolve(percent_resolution_size, no_calc)
                .maybe_add(box_sizing_adjustment),
            aspect_ratio,
            inset: Rect {
                left: inset.left.maybe_resolve(inner.width, no_calc),
                right: inset.right.maybe_resolve(inner.width, no_calc),
                top: inset.top.maybe_resolve(inner.height, no_calc),
                bottom: inset.bottom.maybe_resolve(inner.height, no_calc),
            },
            margin: child_style.margin().resolve_or_zero(inner_width, no_calc),
            margin_is_auto: child_style.margin().map(|m| m.is_auto()),
            padding: child_style.padding().resolve_or_zero(inner_width, no_calc),
            border: child_style.border().resolve_or_zero(inner_width, no_calc),
            align_self: resolve_self_relative(
                child_style.align_self().unwrap_or(constants.align_items),
                child_style.direction(),
                constants.layout_direction,
                constants.is_column,
            ),
            overflow: child_style.overflow(),
            contain: child_style.contain(),
            scrollbar_width: child_style.scrollbar_width(),
            flex_grow: child_style.flex_grow(),
            flex_shrink: child_style.flex_shrink(),
            flex_basis_is_definite: false,
            flex_basis: 0.0,
            inner_flex_basis: 0.0,
            violation: 0.0,
            frozen: false,
            resolved_minimum_main_size: 0.0,
            hypothetical_inner_size: Size::zero(),
            hypothetical_outer_size: Size::zero(),
            target_size: Size::zero(),
            outer_target_size: Size::zero(),
            content_flex_fraction: 0.0,
            baseline: 0.0,
            offset_main: 0.0,
            offset_cross: 0.0,
        });
    }
}

/// 9.2 step 2: the available main and cross space for the items.
fn determine_available_space(
    known_dimensions: Size<Option<f32>>,
    outer_available_space: Size<AvailableSpace>,
    constants: &AlgoConstants,
) -> Size<AvailableSpace> {
    let width = match known_dimensions.width {
        Some(node_width) => {
            AvailableSpace::Definite(node_width - constants.content_box_inset.horizontal_axis_sum())
        }
        None => outer_available_space
            .width
            .maybe_sub(constants.margin.horizontal_axis_sum())
            .maybe_sub(constants.content_box_inset.horizontal_axis_sum()),
    };
    let height = match known_dimensions.height {
        Some(node_height) => {
            AvailableSpace::Definite(node_height - constants.content_box_inset.vertical_axis_sum())
        }
        None => outer_available_space
            .height
            .maybe_sub(constants.margin.vertical_axis_sum())
            .maybe_sub(constants.content_box_inset.vertical_axis_sum()),
    };
    Size { width, height }
}

/// 9.2 step 3: the flex base size and hypothetical main size of each
/// item.
fn determine_flex_base_size(
    tree: &mut impl LayoutTree,
    constants: &AlgoConstants,
    available_space: Size<AvailableSpace>,
    flex_items: &mut [FlexItem],
) {
    let dir = constants.dir;

    for child in flex_items.iter_mut() {
        let cross_axis_parent_size = constants.node_inner_size.cross(dir);
        let child_parent_size = Size::from_cross(dir, cross_axis_parent_size);

        // Min and max sizes transferred through the aspect ratio count.
        let cross_axis_margin_sum = constants.margin.cross_axis_sum(dir);
        let transferred_min_size = child.min_size.maybe_apply_aspect_ratio(child.aspect_ratio);
        let transferred_max_size = child.max_size.maybe_apply_aspect_ratio(child.aspect_ratio);
        let child_min_cross = transferred_min_size
            .cross(dir)
            .maybe_add(cross_axis_margin_sum);
        let child_max_cross = transferred_max_size
            .cross(dir)
            .maybe_add(cross_axis_margin_sum);

        let cross_axis_available_space: AvailableSpace = match available_space.cross(dir) {
            AvailableSpace::Definite(val) => AvailableSpace::Definite(
                cross_axis_parent_size
                    .unwrap_or(val)
                    .maybe_clamp(child_min_cross, child_max_cross),
            ),
            AvailableSpace::MinContent => match child_min_cross {
                Some(min) => AvailableSpace::Definite(min),
                None => AvailableSpace::MinContent,
            },
            AvailableSpace::MaxContent => match child_max_cross {
                Some(max) => AvailableSpace::Definite(max),
                None => AvailableSpace::MaxContent,
            },
        };

        let mut child_cross_size_is_definite = child.size.cross(dir).is_some();
        let child_known_dimensions = {
            let mut ckd = child.size.with_main(dir, None);
            // The definite cross size, clamped, so sizes transferred
            // through the aspect ratio use the used cross size.
            ckd.set_cross(
                dir,
                ckd.cross(dir).maybe_clamp(
                    transferred_min_size.cross(dir),
                    transferred_max_size.cross(dir),
                ),
            );
            if child.align_self == AlignSelf::STRETCH
                && !child.margin_is_auto.cross_start(dir)
                && !child.margin_is_auto.cross_end(dir)
                && ckd.cross(dir).is_none()
            {
                ckd.set_cross(
                    dir,
                    cross_axis_available_space
                        .into_option()
                        .maybe_sub(child.margin.cross_axis_sum(dir)),
                );
                // A stretched cross size is definite in a single-line
                // container with a definite cross size.
                child_cross_size_is_definite = !constants.is_wrap
                    && constants.has_definite_cross_size
                    && cross_axis_parent_size.is_some();
            }
            ckd
        };

        let (box_sizing_adjustment, flex_basis_style) = {
            let child_style = tree.row(child.node);
            let container_width = constants.node_inner_size.main(dir);
            let adjustment = if child_style.box_sizing() == BoxSizing::ContentBox {
                let padding = child_style
                    .padding()
                    .resolve_or_zero(container_width, no_calc);
                let border = child_style
                    .border()
                    .resolve_or_zero(container_width, no_calc);
                (padding + border).sum_axes()
            } else {
                Size::ZERO
            }
            .main(dir);
            (adjustment, child_style.flex_basis())
        };
        // Percentage bases resolve against a definite inner main size.
        let percent_resolution_main_size = if constants.known_main_size_is_definite {
            constants.node_inner_size.main(dir)
        } else {
            None
        };
        let flex_basis = flex_basis_style
            .maybe_resolve(percent_resolution_main_size, no_calc)
            .maybe_add(box_sizing_adjustment);

        child.flex_basis = 'flex_basis: {
            // A: a definite used flex basis. B: `child.size` already holds
            // the aspect-ratio transfer, so the main size covers it.
            let main_size = child.size.main(dir);
            let main_stretch_size =
                percent_resolution_main_size.maybe_sub(child.margin.main_axis_sum(dir));

            // A keyword basis replaces the main size: `stretch` is exact;
            // the others set the measure constraint; unresolved acts as
            // `content`.
            let keyword_main_available_space = if flex_basis_style.is_content() {
                None
            } else if flex_basis_style.is_sizing_keyword() {
                match resolve_sizing_keyword(
                    flex_basis_style,
                    main_stretch_size,
                    percent_resolution_main_size,
                ) {
                    Some(SizingKeywordResolution::Exact(size)) => {
                        child.flex_basis_is_definite = true;
                        break 'flex_basis size;
                    }
                    Some(SizingKeywordResolution::Measure(available)) => Some(available),
                    None => None,
                }
            } else {
                if let Some(flex_basis) = flex_basis.or(main_size) {
                    child.flex_basis_is_definite = true;
                    break 'flex_basis flex_basis;
                };
                match resolve_sizing_keyword(
                    child.size_style.main(dir),
                    main_stretch_size,
                    percent_resolution_main_size,
                ) {
                    Some(SizingKeywordResolution::Exact(size)) => {
                        child.flex_basis_is_definite = true;
                        break 'flex_basis size;
                    }
                    Some(SizingKeywordResolution::Measure(available)) => Some(available),
                    None => None,
                }
            };

            // B, with a definite cross size: transfer it.
            if child_cross_size_is_definite
                && let (Some(ratio), Some(cross)) =
                    (child.aspect_ratio, child_known_dimensions.cross(dir))
            {
                child.flex_basis_is_definite = true;
                break 'flex_basis if dir.is_row() {
                    cross * ratio
                } else {
                    cross / ratio
                };
            }

            // C and E: measure under the constraint (content as
            // max-content).
            let child_available_space = Size::MAX_CONTENT
                .with_main(
                    dir,
                    keyword_main_available_space.unwrap_or(
                        if available_space.main(dir) == AvailableSpace::MinContent {
                            AvailableSpace::MinContent
                        } else {
                            AvailableSpace::MaxContent
                        },
                    ),
                )
                .with_cross(dir, cross_axis_available_space);

            break 'flex_basis measure_child_size(
                tree,
                child.node,
                child_known_dimensions,
                child_parent_size,
                child_available_space,
                SizingMode::ContentSize,
                dir.main_axis(),
            );
        };

        // Padding and border floor the flex basis (as browsers do).
        let padding_border_sum = child.padding.main_axis_sum(dir) + child.border.main_axis_sum(dir);
        child.flex_basis = child.flex_basis.max(padding_border_sum);

        child.inner_flex_basis =
            child.flex_basis - child.padding.main_axis_sum(dir) - child.border.main_axis_sum(dir);

        let padding_border_axes_sums = (child.padding + child.border).sum_axes().map(Some);

        // The parent size stays unset in the main axis: a percentage
        // there must not add to a min-content contribution.
        let automatic_min: Size<Option<f32>> = child
            .overflow
            .map(|overflow| overflow.is_scroll_container().then_some(0.0))
            .into();
        let style_min_main_size = child.min_size.or(automatic_min).main(dir);

        child.resolved_minimum_main_size = style_min_main_size.unwrap_or_else(|| {
            let min_content_main_size = {
                let child_available_space =
                    Size::MIN_CONTENT.with_cross(dir, cross_axis_available_space);
                measure_child_size(
                    tree,
                    child.node,
                    child_known_dimensions,
                    child_parent_size,
                    child_available_space,
                    SizingMode::ContentSize,
                    dir.main_axis(),
                )
            };
            // 4.5: the automatic minimum size.
            let clamped_min_content_size = min_content_main_size
                .maybe_min(child.size.main(dir))
                .maybe_min(transferred_max_size.main(dir));
            clamped_min_content_size.maybe_max(padding_border_axes_sums.main(dir))
        });

        // Transferred sizes clamp the hypothetical main size only.
        let hypothetical_inner_min_main = child
            .resolved_minimum_main_size
            .maybe_max(transferred_min_size.main(dir))
            .maybe_max(padding_border_axes_sums.main(dir));
        let hypothetical_inner_size = child.flex_basis.maybe_clamp(
            Some(hypothetical_inner_min_main),
            transferred_max_size.main(dir),
        );
        let hypothetical_outer_size = hypothetical_inner_size + child.margin.main_axis_sum(dir);

        child
            .hypothetical_inner_size
            .set_main(dir, hypothetical_inner_size);
        child
            .hypothetical_outer_size
            .set_main(dir, hypothetical_outer_size);
    }
}

/// 9.3 step 5: collect items into lines.
fn collect_flex_lines(
    constants: &AlgoConstants,
    available_space: Size<AvailableSpace>,
    flex_items: &[FlexItem],
    lines: &mut Vec<FlexLine>,
) {
    let dir = constants.dir;
    // Wrapping needs a definite main size; a content-derived known size
    // keeps one line, as the container was sized that way.
    if !constants.is_wrap || !constants.known_main_size_is_definite {
        lines.push(FlexLine::new(0, flex_items.len()));
        return;
    }
    let main_axis_available_space = match constants.max_size.main(dir) {
        Some(max_size) => AvailableSpace::Definite({
            let available = available_space.main(dir).into_option().unwrap_or(max_size);
            // An indefinite main size is at most the max size.
            let available = if constants.has_definite_main_size {
                available
            } else {
                available.min(max_size)
            };
            available.maybe_max(constants.min_size.main(dir))
        }),
        None => available_space.main(dir),
    };

    match main_axis_available_space {
        // Under max-content nothing wraps.
        AvailableSpace::MaxContent => lines.push(FlexLine::new(0, flex_items.len())),
        // Under min-content every item takes a line.
        AvailableSpace::MinContent => {
            for i in 0..flex_items.len() {
                lines.push(FlexLine::new(i, i + 1));
            }
        }
        AvailableSpace::Definite(main_axis_available_space) => {
            let main_axis_gap = constants.gap.main(dir);
            let mut start = 0;
            while start < flex_items.len() {
                // The first item of the next line, or the end.
                let mut line_length = 0.0;
                let index = flex_items[start..]
                    .iter()
                    .enumerate()
                    .find(|&(idx, child)| {
                        // Gaps are between items only.
                        let gap_contribution = if idx == 0 { 0.0 } else { main_axis_gap };
                        line_length += child.hypothetical_outer_size.main(dir) + gap_contribution;
                        line_length > main_axis_available_space && idx != 0
                    })
                    .map(|(idx, _)| idx)
                    .unwrap_or(flex_items.len() - start);
                lines.push(FlexLine::new(start, start + index));
                start += index;
            }
        }
    }
}

/// Whether an item's known dimensions are definite for its own layout.
/// The main size is definite if the container's is or the item's basis
/// is; the cross size if stretched, set, or (for a column's width) fit
/// into definite available space.
fn item_known_dimension_definiteness(constants: &AlgoConstants, item: &FlexItem) -> Size<bool> {
    let dir = constants.dir;
    let main_is_definite = constants.has_definite_main_size || item.flex_basis_is_definite;
    let has_cross_auto_margins =
        item.margin_is_auto.cross_start(dir) || item.margin_is_auto.cross_end(dir);
    let cross_size = item.size_style.cross(dir);
    let is_stretched = !has_cross_auto_margins
        && (cross_size.is_stretch()
            || (item.align_self == AlignSelf::STRETCH && cross_size.is_auto()));
    let cross_is_definite = is_stretched
        || item.size.cross(dir).is_some()
        || (!dir.is_row() && constants.cross_axis_available_space_is_definite);
    Size {
        width: true,
        height: true,
    }
    .with_main(dir, main_is_definite)
    .with_cross(dir, cross_is_definite)
}

fn item_main_length(child: &FlexItem, dir: FlexDirection) -> f32 {
    let padding_border_sum = (child.padding + child.border).main_axis_sum(dir);
    (child.flex_basis.maybe_max(child.min_size.main(dir)) + child.margin.main_axis_sum(dir))
        .max(padding_border_sum)
}

/// The container's main size, when not known.
fn determine_container_main_size(
    tree: &mut impl LayoutTree,
    available_space: Size<AvailableSpace>,
    items: &mut [FlexItem],
    lines: &[FlexLine],
    constants: &mut AlgoConstants,
) {
    let dir = constants.dir;
    let main_content_box_inset = constants.content_box_inset.main_axis_sum(dir);

    let outer_main_size: f32 = match constants.node_outer_size.main(dir) {
        Some(size) => size,
        None => match available_space.main(dir) {
            AvailableSpace::Definite(main_axis_available_space) => {
                let main_axis_gap = constants.gap.main(dir);
                let longest_line_length: f32 = lines
                    .iter()
                    .map(|line| {
                        let line_main_axis_gap = sum_axis_gaps(main_axis_gap, line.len());
                        let total_target_size = items[line.start..line.end]
                            .iter()
                            .map(|child| item_main_length(child, dir))
                            .sum::<f32>();
                        total_target_size + line_main_axis_gap
                    })
                    .max_by(|a, b| a.total_cmp(b))
                    .unwrap_or(0.0);
                let size = longest_line_length + main_content_box_inset;
                if lines.len() > 1 {
                    size.max(main_axis_available_space)
                } else {
                    size
                }
            }
            AvailableSpace::MinContent if constants.is_wrap => {
                let longest_line_length: f32 = lines
                    .iter()
                    .map(|line| {
                        let line_main_axis_gap = sum_axis_gaps(constants.gap.main(dir), line.len());
                        let total_target_size = items[line.start..line.end]
                            .iter()
                            .map(|child| item_main_length(child, dir))
                            .sum::<f32>();
                        total_target_size + line_main_axis_gap
                    })
                    .max_by(|a, b| a.total_cmp(b))
                    .unwrap_or(0.0);
                longest_line_length + main_content_box_inset
            }
            AvailableSpace::MinContent | AvailableSpace::MaxContent => {
                // The largest sum of item contributions over a line.
                let mut main_size = 0.0;
                for line in lines.iter() {
                    for item in items[line.start..line.end].iter_mut() {
                        let content_contribution =
                            content_contribution(tree, item, available_space, constants);
                        item.content_flex_fraction = {
                            let diff = content_contribution - item.flex_basis;
                            if diff > 0.0 {
                                diff / 1.0f32.max(item.flex_grow)
                            } else if diff < 0.0 {
                                let scaled_shrink_factor =
                                    1.0f32.max(item.flex_shrink) * item.inner_flex_basis;
                                diff / scaled_shrink_factor
                            } else {
                                0.0
                            }
                        };
                    }

                    // Taffy (and browsers) skip the spec's scaling by the
                    // line's largest flex fraction.
                    let item_main_size_sum = items[line.start..line.end]
                        .iter_mut()
                        .map(|item| {
                            let flex_fraction = item.content_flex_fraction;
                            let flex_contribution = if item.content_flex_fraction > 0.0 {
                                1.0f32.max(item.flex_grow) * flex_fraction
                            } else if item.content_flex_fraction < 0.0 {
                                let scaled_shrink_factor =
                                    1.0f32.max(item.flex_shrink) * item.inner_flex_basis;
                                if scaled_shrink_factor == 0.0 {
                                    0.0
                                } else {
                                    scaled_shrink_factor * flex_fraction
                                }
                            } else {
                                0.0
                            };
                            let size = item.flex_basis + flex_contribution;
                            item.outer_target_size.set_main(dir, size);
                            item.target_size.set_main(dir, size);
                            size
                        })
                        .sum::<f32>();

                    let gap_sum = sum_axis_gaps(constants.gap.main(dir), line.len());
                    main_size = f32::max(main_size, item_main_size_sum + gap_sum)
                }
                main_size + main_content_box_inset
            }
        },
    };

    let outer_main_size = outer_main_size
        .maybe_clamp(constants.min_size.main(dir), constants.max_size.main(dir))
        .max(main_content_box_inset - constants.scrollbar_gutter.main(dir));
    let inner_main_size = (outer_main_size - main_content_box_inset).max(0.0);
    constants.container_size.set_main(dir, outer_main_size);
    constants
        .inner_container_size
        .set_main(dir, inner_main_size);
    constants
        .node_inner_size
        .set_main(dir, Some(inner_main_size));
}

/// An item's min- or max-content contribution to the container's main
/// size.
fn content_contribution(
    tree: &mut impl LayoutTree,
    item: &FlexItem,
    available_space: Size<AvailableSpace>,
    constants: &AlgoConstants,
) -> f32 {
    let dir = constants.dir;
    let style_min = item.min_size.main(dir);
    let style_preferred = item.size.main(dir);
    let style_max = item.max_size.main(dir);

    // Browsers clamp by the preferred size too (csswg-drafts #1435).
    let clamping_basis = Some(item.flex_basis).maybe_max(style_preferred);
    let flex_basis_min = clamping_basis.filter(|_| item.flex_shrink == 0.0);
    let flex_basis_max = clamping_basis.filter(|_| item.flex_grow == 0.0);

    let min_main_size = style_min
        .maybe_max(flex_basis_min)
        .or(flex_basis_min)
        .unwrap_or(item.resolved_minimum_main_size)
        .max(item.resolved_minimum_main_size);
    let max_main_size = style_max
        .maybe_min(flex_basis_max)
        .or(flex_basis_max)
        .unwrap_or(f32::INFINITY);

    match (min_main_size, style_preferred, max_main_size) {
        // The clamps decide: skip the measure.
        (min, Some(pref), max) if max <= min || max <= pref => {
            pref.min(max).max(min) + item.margin.main_axis_sum(dir)
        }
        (min, _, max) if max <= min => min + item.margin.main_axis_sum(dir),
        _ if item.is_scroll_container() => item.flex_basis + item.margin.main_axis_sum(dir),
        // A definite preferred size is the contribution, floored by
        // padding and border.
        (_, Some(pref), _) => {
            let item_pb_main = item.padding.main_axis_sum(dir) + item.border.main_axis_sum(dir);
            let content_main_size = pref.max(item_pb_main) + item.margin.main_axis_sum(dir);
            if constants.is_row {
                content_main_size.maybe_clamp(style_min, style_max)
            } else {
                content_main_size
                    .max(item.flex_basis)
                    .maybe_clamp(style_min, style_max)
            }
        }
        _ => {
            let cross_axis_parent_size = constants.node_inner_size.cross(dir);
            let cross_axis_margin_sum = constants.margin.cross_axis_sum(dir);
            let child_min_cross = item.min_size.cross(dir).maybe_add(cross_axis_margin_sum);
            let child_max_cross = item.max_size.cross(dir).maybe_add(cross_axis_margin_sum);
            let cross_axis_available_space: AvailableSpace = available_space
                .cross(dir)
                .map_definite_value(|val| cross_axis_parent_size.unwrap_or(val))
                .maybe_clamp(child_min_cross, child_max_cross);
            let child_available_space = available_space.with_cross(dir, cross_axis_available_space);
            let child_known_dimensions = {
                let mut ckd = item.size.with_main(dir, None);
                if item.align_self == AlignSelf::STRETCH && ckd.cross(dir).is_none() {
                    ckd.set_cross(
                        dir,
                        cross_axis_available_space
                            .into_option()
                            .maybe_sub(item.margin.cross_axis_sum(dir)),
                    );
                }
                ckd
            };
            let measured_main_size = measure_child_size(
                tree,
                item.node,
                child_known_dimensions,
                constants.node_inner_size,
                child_available_space,
                SizingMode::ContentSize,
                dir.main_axis(),
            );
            // A known cross size, through the aspect ratio, floors the
            // measure.
            let transferred_main_size = item
                .aspect_ratio
                .zip(child_known_dimensions.cross(dir))
                .map(|(ratio, cross)| {
                    if constants.is_row {
                        cross * ratio
                    } else {
                        cross / ratio
                    }
                });
            let content_main_size = measured_main_size.maybe_max(transferred_main_size)
                + item.margin.main_axis_sum(dir);
            // Rows and columns differ here, as in browsers (Taffy's
            // `flex_basis_unconstraint_*` tests).
            if constants.is_row {
                content_main_size.maybe_clamp(style_min, style_max)
            } else {
                content_main_size
                    .max(item.flex_basis)
                    .maybe_clamp(style_min, style_max)
            }
        }
    }
}

/// 9.7: resolve the flexible lengths of one line's items.
fn resolve_flexible_lengths(items: &mut [FlexItem], constants: &AlgoConstants) {
    let dir = constants.dir;
    let total_main_axis_gap = sum_axis_gaps(constants.gap.main(dir), items.len());

    // 1: the used flex factor.
    let total_hypothetical_outer_main_size = items
        .iter()
        .map(|child| child.hypothetical_outer_size.main(dir))
        .sum::<f32>();
    let used_flex_factor: f32 = total_main_axis_gap + total_hypothetical_outer_main_size;
    let inner_main = constants.node_inner_size.main(dir);
    let growing = used_flex_factor < inner_main.unwrap_or(0.0);
    let shrinking = used_flex_factor > inner_main.unwrap_or(0.0);
    let exactly_sized = !growing & !shrinking;

    // 2: freeze inflexible items.
    for child in items.iter_mut() {
        let inner_target_size = child.hypothetical_inner_size.main(dir);
        child.target_size.set_main(dir, inner_target_size);
        if exactly_sized
            || (child.flex_grow == 0.0 && child.flex_shrink == 0.0)
            || (growing && child.flex_basis > child.hypothetical_inner_size.main(dir))
            || (shrinking && child.flex_basis < child.hypothetical_inner_size.main(dir))
        {
            child.frozen = true;
            let outer_target_size = inner_target_size + child.margin.main_axis_sum(dir);
            child.outer_target_size.set_main(dir, outer_target_size);
        }
    }
    if exactly_sized {
        return;
    }

    // 3: the initial free space.
    let used_space = |items: &[FlexItem]| -> f32 {
        total_main_axis_gap
            + items
                .iter()
                .map(|child| {
                    if child.frozen {
                        child.outer_target_size.main(dir)
                    } else {
                        child.flex_basis + child.margin.main_axis_sum(dir)
                    }
                })
                .sum::<f32>()
    };
    let initial_free_space = inner_main.maybe_sub(used_space(items)).unwrap_or(0.0);

    // 4: loop.
    loop {
        // a: all frozen: done.
        if items.iter().all(|child| child.frozen) {
            break;
        }

        // b: the remaining free space.
        let used_space = used_space(items);
        let (sum_flex_grow, sum_flex_shrink): (f32, f32) = items
            .iter()
            .filter(|child| !child.frozen)
            .fold((0.0, 0.0), |(flex_grow, flex_shrink), item| {
                (flex_grow + item.flex_grow, flex_shrink + item.flex_shrink)
            });
        let free_space = if growing && sum_flex_grow < 1.0 {
            (initial_free_space * sum_flex_grow - total_main_axis_gap)
                .maybe_min(inner_main.maybe_sub(used_space))
        } else if shrinking && sum_flex_shrink < 1.0 {
            (initial_free_space * sum_flex_shrink - total_main_axis_gap)
                .maybe_max(inner_main.maybe_sub(used_space))
        } else {
            (inner_main.maybe_sub(used_space)).unwrap_or(used_flex_factor - used_space)
        };

        // c: distribute it by the flex factors.
        if free_space.is_normal() {
            if growing && sum_flex_grow > 0.0 {
                for child in items.iter_mut().filter(|child| !child.frozen) {
                    child.target_size.set_main(
                        dir,
                        child.flex_basis + free_space * (child.flex_grow / sum_flex_grow),
                    );
                }
            } else if shrinking && sum_flex_shrink > 0.0 {
                let sum_scaled_shrink_factor: f32 = items
                    .iter()
                    .filter(|child| !child.frozen)
                    .map(|child| child.inner_flex_basis * child.flex_shrink)
                    .sum();
                if sum_scaled_shrink_factor > 0.0 {
                    for child in items.iter_mut().filter(|child| !child.frozen) {
                        let scaled_shrink_factor = child.inner_flex_basis * child.flex_shrink;
                        child.target_size.set_main(
                            dir,
                            child.flex_basis
                                + free_space * (scaled_shrink_factor / sum_scaled_shrink_factor),
                        )
                    }
                }
            }
        }

        // d: fix min and max violations.
        let total_violation =
            items
                .iter_mut()
                .filter(|child| !child.frozen)
                .fold(0.0, |acc, child| -> f32 {
                    let resolved_min_main: Option<f32> = child.resolved_minimum_main_size.into();
                    let max_main = child.max_size.main(dir);
                    let clamped = child
                        .target_size
                        .main(dir)
                        .maybe_clamp(resolved_min_main, max_main)
                        .max(0.0);
                    child.violation = clamped - child.target_size.main(dir);
                    child.target_size.set_main(dir, clamped);
                    child.outer_target_size.set_main(
                        dir,
                        child.target_size.main(dir) + child.margin.main_axis_sum(dir),
                    );
                    acc + child.violation
                });

        // e: freeze over-flexed items.
        for child in items.iter_mut().filter(|child| !child.frozen) {
            match total_violation {
                v if v > 0.0 => child.frozen = child.violation > 0.0,
                v if v < 0.0 => child.frozen = child.violation < 0.0,
                _ => child.frozen = true,
            }
        }
    }
}

/// 9.4 step 7: each item's hypothetical cross size, with auto as
/// fit-content.
fn determine_hypothetical_cross_size(
    tree: &mut impl LayoutTree,
    items: &mut [FlexItem],
    constants: &AlgoConstants,
    available_space: Size<AvailableSpace>,
) {
    let dir = constants.dir;
    for child in items.iter_mut() {
        let padding_border_sum = (child.padding + child.border).cross_axis_sum(dir);
        let child_known_main = constants.container_size.main(dir).into();

        // Transferred sizes clamp the hypothetical cross size.
        let transferred_min_cross = child
            .min_size
            .maybe_apply_aspect_ratio(child.aspect_ratio)
            .cross(dir);
        let transferred_max_cross = child
            .max_size
            .maybe_apply_aspect_ratio(child.aspect_ratio)
            .cross(dir);

        let child_cross = child
            .size
            .cross(dir)
            .maybe_clamp(transferred_min_cross, transferred_max_cross)
            .maybe_max(padding_border_sum);

        let child_available_cross = available_space
            .cross(dir)
            .maybe_clamp(transferred_min_cross, transferred_max_cross)
            .maybe_max(padding_border_sum);

        // A keyword cross size sets the measure constraint; `stretch`
        // waits for the line (`determine_used_cross_size`).
        let cross_stretch_size = constants
            .node_inner_size
            .cross(dir)
            .maybe_sub(child.margin.cross_axis_sum(dir));
        let child_available_cross = match resolve_sizing_keyword(
            child.size_style.cross(dir),
            cross_stretch_size,
            constants.node_inner_size.cross(dir),
        ) {
            Some(SizingKeywordResolution::Measure(available)) => available,
            _ => child_available_cross,
        };

        let child_inner_cross = child_cross.unwrap_or_else(|| {
            tree.compute_child(
                child.node,
                LayoutInput {
                    run_mode: RunMode::ComputeSize,
                    sizing_mode: SizingMode::ContentSize,
                    axis: dir.cross_axis().into(),
                    known_dimensions: Size {
                        width: if constants.is_row {
                            child.target_size.width.into()
                        } else {
                            child_cross
                        },
                        height: if constants.is_row {
                            child_cross
                        } else {
                            child.target_size.height.into()
                        },
                    },
                    known_dimensions_are_definite: item_known_dimension_definiteness(
                        constants, child,
                    ),
                    parent_size: constants.node_inner_size,
                    available_space: Size {
                        width: if constants.is_row {
                            child_known_main
                        } else {
                            child_available_cross
                        },
                        height: if constants.is_row {
                            child_available_cross
                        } else {
                            child_known_main
                        },
                    },
                    vertical_margins_are_collapsible: Line::FALSE,
                },
            )
            .size
            .get_abs(dir.cross_axis())
            .maybe_clamp(transferred_min_cross, transferred_max_cross)
            .max(padding_border_sum)
        });
        let child_outer_cross = child_inner_cross + child.margin.cross_axis_sum(dir);

        child
            .hypothetical_inner_size
            .set_cross(dir, child_inner_cross);
        child
            .hypothetical_outer_size
            .set_cross(dir, child_outer_cross);
    }
}

/// Baselines of the items that take part in baseline alignment (rows
/// only: the cross axis is the inline axis there).
fn calculate_children_base_lines(
    tree: &mut impl LayoutTree,
    node_size: Size<Option<f32>>,
    available_space: Size<AvailableSpace>,
    items: &mut [FlexItem],
    lines: &[FlexLine],
    constants: &AlgoConstants,
) {
    let dir = constants.dir;
    if !constants.is_row {
        return;
    }
    for line in lines {
        let line_items = &mut items[line.start..line.end];
        // One participant or none: nothing to align.
        let line_baseline_child_count = line_items
            .iter()
            .filter(|child| child.participates_in_baseline_alignment(dir))
            .count();
        if line_baseline_child_count <= 1 {
            continue;
        }
        for child in line_items.iter_mut() {
            if !child.participates_in_baseline_alignment(dir) {
                continue;
            }
            let measured_size_and_baselines = tree.compute_child(
                child.node,
                LayoutInput {
                    run_mode: RunMode::PerformLayout,
                    sizing_mode: SizingMode::ContentSize,
                    axis: RequestedAxis::Both,
                    known_dimensions: Size {
                        width: if constants.is_row {
                            child.target_size.width.into()
                        } else {
                            child.hypothetical_inner_size.width.into()
                        },
                        height: if constants.is_row {
                            child.hypothetical_inner_size.height.into()
                        } else {
                            child.target_size.height.into()
                        },
                    },
                    known_dimensions_are_definite: item_known_dimension_definiteness(
                        constants, child,
                    ),
                    parent_size: constants.node_inner_size,
                    available_space: Size {
                        width: if constants.is_row {
                            constants.container_size.width.into()
                        } else {
                            available_space.width.maybe_set(node_size.width)
                        },
                        height: if constants.is_row {
                            available_space.height.maybe_set(node_size.height)
                        } else {
                            constants.container_size.height.into()
                        },
                    },
                    vertical_margins_are_collapsible: Line::FALSE,
                },
            );
            let baseline = measured_size_and_baselines.baselines.first;
            let height = measured_size_and_baselines.size.height;
            // A scroll container's baseline is clamped to its border box
            // (csswg-drafts #7660).
            let baseline = if child.overflow.y.is_scroll_container() {
                baseline.unwrap_or(height).min(height).max(0.0)
            } else {
                baseline.unwrap_or(height)
            };
            child.baseline = baseline + child.margin.top;
        }
    }
}

/// 9.4 step 8: the cross size of each line.
fn calculate_cross_size(
    items: &[FlexItem],
    lines: &mut [FlexLine],
    node_size: Size<Option<f32>>,
    constants: &AlgoConstants,
) {
    let dir = constants.dir;
    // Single line, definite cross size: the line takes the inner size.
    if !constants.is_wrap && node_size.cross(dir).is_some() {
        let cross_axis_padding_border = constants.content_box_inset.cross_axis_sum(dir);
        let cross_min_size = constants.min_size.cross(dir);
        let cross_max_size = constants.max_size.cross(dir);
        lines[0].cross_size = node_size
            .cross(dir)
            .maybe_clamp(cross_min_size, cross_max_size)
            .maybe_sub(cross_axis_padding_border)
            .maybe_max(0.0)
            .unwrap_or(0.0);
    } else {
        // The largest outer hypothetical cross size, with baseline
        // participants measured from the shared baseline.
        for line in lines.iter_mut() {
            let line_items = &items[line.start..line.end];
            let max_baseline: f32 = line_items
                .iter()
                .map(|child| child.baseline)
                .fold(0.0, |acc, x| acc.max(x));
            line.cross_size = line_items
                .iter()
                .map(|child| {
                    if child.participates_in_baseline_alignment(dir) {
                        max_baseline - child.baseline + child.hypothetical_outer_size.cross(dir)
                    } else {
                        child.hypothetical_outer_size.cross(dir)
                    }
                })
                .fold(0.0, |acc, x| acc.max(x));
        }
        // A single line clamps to the container's min and max.
        if !constants.is_wrap {
            let cross_axis_padding_border = constants.content_box_inset.cross_axis_sum(dir);
            let cross_min_size = constants.min_size.cross(dir);
            let cross_max_size = constants.max_size.cross(dir);
            lines[0].cross_size = lines[0].cross_size.maybe_clamp(
                cross_min_size.maybe_sub(cross_axis_padding_border),
                cross_max_size.maybe_sub(cross_axis_padding_border),
            );
        }
    }
}

/// 9.4 step 9: `align-content: stretch` grows the lines to fill a
/// definite cross size.
fn handle_align_content_stretch(
    lines: &mut [FlexLine],
    node_size: Size<Option<f32>>,
    constants: &AlgoConstants,
) {
    let dir = constants.dir;
    if constants.align_content == AlignContent::STRETCH {
        let cross_axis_padding_border = constants.content_box_inset.cross_axis_sum(dir);
        let cross_min_size = constants.min_size.cross(dir);
        let cross_max_size = constants.max_size.cross(dir);
        let container_min_inner_cross = node_size
            .cross(dir)
            .or(cross_min_size)
            .maybe_clamp(cross_min_size, cross_max_size)
            .maybe_sub(cross_axis_padding_border)
            .maybe_max(0.0)
            .unwrap_or(0.0);

        let total_cross_axis_gap = sum_axis_gaps(constants.gap.cross(dir), lines.len());
        let lines_total_cross: f32 =
            lines.iter().map(|line| line.cross_size).sum::<f32>() + total_cross_axis_gap;

        if lines_total_cross < container_min_inner_cross {
            let remaining = container_min_inner_cross - lines_total_cross;
            let addition = remaining / lines.len() as f32;
            lines
                .iter_mut()
                .for_each(|line| line.cross_size += addition);
        }
    }
}

/// 9.4 step 11: each item's used cross size (stretched items take the
/// line).
fn determine_used_cross_size(
    tree: &impl LayoutTree,
    items: &mut [FlexItem],
    lines: &[FlexLine],
    constants: &AlgoConstants,
) {
    let dir = constants.dir;
    for line in lines {
        let line_cross_size = line.cross_size;
        for child in items[line.start..line.end].iter_mut() {
            let child_style = tree.row(child.node);
            // A `stretch` cross size stretches regardless of alignment.
            let cross_is_stretch = child.size_style.cross(dir).is_stretch();
            child.target_size.set_cross(
                dir,
                if !child.margin_is_auto.cross_start(dir)
                    && !child.margin_is_auto.cross_end(dir)
                    && (cross_is_stretch
                        || (child.align_self == AlignSelf::STRETCH
                            && child_style.size().cross(dir).is_auto()))
                {
                    // The max size here does not transfer through the
                    // aspect ratio (as in Chrome and Firefox).
                    let padding = child_style
                        .padding()
                        .resolve_or_zero(constants.node_inner_size, no_calc);
                    let border = child_style
                        .border()
                        .resolve_or_zero(constants.node_inner_size, no_calc);
                    let pb_sum = (padding + border).sum_axes();
                    let box_sizing_adjustment = if child_style.box_sizing() == BoxSizing::ContentBox
                    {
                        pb_sum
                    } else {
                        Size::ZERO
                    };
                    let max_size_ignoring_aspect_ratio = child_style
                        .max_size()
                        .maybe_resolve(constants.node_inner_size, no_calc)
                        .maybe_add(box_sizing_adjustment);
                    (line_cross_size - child.margin.cross_axis_sum(dir)).maybe_clamp(
                        child.min_size.cross(dir),
                        max_size_ignoring_aspect_ratio.cross(dir),
                    )
                } else {
                    child.hypothetical_inner_size.cross(dir)
                },
            );
            child.outer_target_size.set_cross(
                dir,
                child.target_size.cross(dir) + child.margin.cross_axis_sum(dir),
            );
        }
    }
}

/// 9.5 step 12: main-axis auto margins, then `justify-content`.
fn distribute_remaining_free_space(
    items: &mut [FlexItem],
    lines: &[FlexLine],
    constants: &AlgoConstants,
) {
    let dir = constants.dir;
    for line in lines {
        let line_items = &mut items[line.start..line.end];
        let total_main_axis_gap = sum_axis_gaps(constants.gap.main(dir), line_items.len());
        let used_space: f32 = total_main_axis_gap
            + line_items
                .iter()
                .map(|child| child.outer_target_size.main(dir))
                .sum::<f32>();
        let mut free_space = constants.inner_container_size.main(dir) - used_space;
        let mut num_auto_margins = 0;

        for child in line_items.iter_mut() {
            if child.margin_is_auto.main_start(dir) {
                num_auto_margins += 1;
            }
            if child.margin_is_auto.main_end(dir) {
                num_auto_margins += 1;
            }
        }

        if free_space > 0.0 && num_auto_margins > 0 {
            let margin = free_space / num_auto_margins as f32;
            for child in line_items.iter_mut() {
                if child.margin_is_auto.main_start(dir) {
                    if constants.is_row {
                        child.margin.left = margin;
                    } else {
                        child.margin.top = margin;
                    }
                }
                if child.margin_is_auto.main_end(dir) {
                    if constants.is_row {
                        child.margin.right = margin;
                    } else {
                        child.margin.bottom = margin;
                    }
                }
            }
            // The auto margins took all of it.
            free_space = 0.0;
        }

        let num_items = line_items.len();
        let layout_reverse = dir.is_reverse();
        let gap = constants.gap.main(dir);
        let raw_justify_content_mode = constants
            .justify_content
            .unwrap_or(JustifyContent::FLEX_START);
        let justify_content_mode =
            apply_alignment_fallback(free_space, num_items, raw_justify_content_mode);

        let justify_item = |(i, child): (usize, &mut FlexItem)| {
            child.offset_main = compute_alignment_offset(
                free_space,
                num_items,
                gap,
                justify_content_mode,
                layout_reverse,
                i == 0,
            );
        };
        if layout_reverse {
            line_items
                .iter_mut()
                .rev()
                .enumerate()
                .for_each(justify_item);
        } else {
            line_items.iter_mut().enumerate().for_each(justify_item);
        }
    }
}

/// 9.6 steps 13 and 14: cross-axis auto margins, then `align-self`.
fn resolve_cross_axis_auto_margins(
    items: &mut [FlexItem],
    lines: &[FlexLine],
    constants: &AlgoConstants,
) {
    let dir = constants.dir;
    for line in lines {
        let line_items = &mut items[line.start..line.end];
        let line_cross_size = line.cross_size;
        let max_baseline: f32 = line_items
            .iter_mut()
            .map(|child| child.baseline)
            .fold(0.0, |acc, x| acc.max(x));
        let max_baseline_to_bottom_distance: f32 = line_items
            .iter_mut()
            .filter(|child| child.participates_in_baseline_alignment(dir))
            .map(|child| child.outer_target_size.cross(dir) - child.baseline)
            .fold(0.0, |acc, x| acc.max(x));

        for child in line_items.iter_mut() {
            let free_space = line_cross_size - child.outer_target_size.cross(dir);
            if child.margin_is_auto.cross_start(dir) && child.margin_is_auto.cross_end(dir) {
                if constants.is_row {
                    child.margin.top = free_space / 2.0;
                    child.margin.bottom = free_space / 2.0;
                } else {
                    child.margin.left = free_space / 2.0;
                    child.margin.right = free_space / 2.0;
                }
            } else if child.margin_is_auto.cross_start(dir) {
                if constants.is_row {
                    child.margin.top = free_space;
                } else {
                    child.margin.left = free_space;
                }
            } else if child.margin_is_auto.cross_end(dir) {
                if constants.is_row {
                    child.margin.bottom = free_space;
                } else {
                    child.margin.right = free_space;
                }
            } else {
                child.offset_cross = align_flex_items_along_cross_axis(
                    child,
                    free_space,
                    max_baseline,
                    max_baseline_to_bottom_distance,
                    constants,
                );
            }
        }
    }
}

/// 9.6 step 14: an item's cross offset per `align-self`.
fn align_flex_items_along_cross_axis(
    child: &FlexItem,
    free_space: f32,
    max_baseline: f32,
    max_baseline_to_bottom_distance: f32,
    constants: &AlgoConstants,
) -> f32 {
    let cross_axis_should_reverse =
        constants.is_column && matches!(constants.layout_direction, Direction::Rtl);

    // `safe` falls back to start when the item overflows its line.
    let align_keyword = if child.align_self.is_safe() && free_space < 0.0 {
        AlignItemsKeyword::Start
    } else {
        child.align_self.keyword
    };

    match align_keyword {
        AlignItemsKeyword::Start => {
            if cross_axis_should_reverse {
                free_space
            } else {
                0.0
            }
        }
        AlignItemsKeyword::FlexStart => {
            if constants.is_wrap_reverse ^ cross_axis_should_reverse {
                free_space
            } else {
                0.0
            }
        }
        AlignItemsKeyword::End => {
            if cross_axis_should_reverse {
                0.0
            } else {
                free_space
            }
        }
        AlignItemsKeyword::FlexEnd => {
            if constants.is_wrap_reverse ^ cross_axis_should_reverse {
                0.0
            } else {
                free_space
            }
        }
        AlignItemsKeyword::Center => free_space / 2.0,
        AlignItemsKeyword::Baseline => {
            if constants.is_row {
                if constants.is_wrap_reverse {
                    // Flipped cross axis: the baseline group aligns to the
                    // bottom of the line.
                    let line_cross_size = free_space + child.outer_target_size.cross(constants.dir);
                    line_cross_size - max_baseline_to_bottom_distance - child.baseline
                } else {
                    max_baseline - child.baseline
                }
            } else {
                // In columns baseline acts as flex-start.
                let baseline_column_should_reverse =
                    cross_axis_should_reverse && !constants.is_wrap;
                if constants.is_wrap_reverse ^ baseline_column_should_reverse {
                    free_space
                } else {
                    0.0
                }
            }
        }
        AlignItemsKeyword::Stretch => {
            if constants.is_wrap_reverse ^ cross_axis_should_reverse {
                free_space
            } else {
                0.0
            }
        }
        // Resolved against the item's direction when items are made.
        AlignItemsKeyword::SelfStart | AlignItemsKeyword::SelfEnd => unreachable!(),
    }
}

/// 9.6 step 15: the container's cross size. Returns the lines' total
/// cross size.
fn determine_container_cross_size(
    lines: &[FlexLine],
    node_size: Size<Option<f32>>,
    constants: &mut AlgoConstants,
) -> f32 {
    let dir = constants.dir;
    let total_cross_axis_gap = sum_axis_gaps(constants.gap.cross(dir), lines.len());
    let total_line_cross_size: f32 = lines.iter().map(|line| line.cross_size).sum::<f32>();

    let padding_border_sum = constants.content_box_inset.cross_axis_sum(dir);
    let cross_scrollbar_gutter = constants.scrollbar_gutter.cross(dir);
    let min_cross_size = constants.min_size.cross(dir);
    let max_cross_size = constants.max_size.cross(dir);
    let outer_container_size = node_size
        .cross(dir)
        .unwrap_or(total_line_cross_size + total_cross_axis_gap + padding_border_sum)
        .maybe_clamp(min_cross_size, max_cross_size)
        .max(padding_border_sum - cross_scrollbar_gutter);
    let inner_container_size = (outer_container_size - padding_border_sum).max(0.0);

    constants
        .container_size
        .set_cross(dir, outer_container_size);
    constants
        .inner_container_size
        .set_cross(dir, inner_container_size);
    total_line_cross_size
}

/// 9.6 step 16: align the lines per `align-content`.
fn align_flex_lines_per_align_content(
    lines: &mut [FlexLine],
    constants: &AlgoConstants,
    total_cross_size: f32,
) {
    let num_lines = lines.len();
    let gap = constants.gap.cross(constants.dir);
    let total_cross_axis_gap = sum_axis_gaps(gap, num_lines);
    let free_space = constants.inner_container_size.cross(constants.dir)
        - total_cross_size
        - total_cross_axis_gap;
    let align_content_mode =
        apply_alignment_fallback(free_space, num_lines, constants.align_content);

    let align_line = |(i, line): (usize, &mut FlexLine)| {
        line.offset_cross = compute_alignment_offset(
            free_space,
            num_lines,
            gap,
            align_content_mode,
            constants.is_wrap_reverse,
            i == 0,
        );
    };
    if constants.is_wrap_reverse {
        lines.iter_mut().rev().enumerate().for_each(align_line);
    } else {
        lines.iter_mut().enumerate().for_each(align_line);
    }
}

/// Lays out one item and records its layout.
#[allow(clippy::too_many_arguments)]
fn calculate_flex_item(
    tree: &mut impl LayoutTree,
    item: &mut FlexItem,
    total_offset_main: &mut f32,
    total_offset_cross: f32,
    line_offset_cross: f32,
    total_overflow_rect: &mut Rect<f32>,
    border: Rect<f32>,
    constants: &AlgoConstants,
) {
    let container_size = constants.container_size;
    let node_inner_size = constants.node_inner_size;
    let direction = constants.dir;
    let layout_direction = constants.layout_direction;
    let definiteness = item_known_dimension_definiteness(constants, item);
    let layout_output = tree.compute_child(
        item.node,
        LayoutInput {
            run_mode: RunMode::PerformLayout,
            sizing_mode: SizingMode::ContentSize,
            axis: RequestedAxis::Both,
            known_dimensions: item.target_size.map(|s| s.into()),
            known_dimensions_are_definite: definiteness,
            parent_size: node_inner_size,
            available_space: container_size.map(|s| s.into()),
            vertical_margins_are_collapsible: Line::FALSE,
        },
    );
    let LayoutOutput {
        size,
        scrollable_overflow_rect,
        ..
    } = layout_output;

    let is_rtl_row = direction.is_row() && layout_direction == Direction::Rtl;
    let is_rtl_column = direction.is_column() && layout_direction == Direction::Rtl;
    let main_relative_inset = if is_rtl_row {
        item.inset
            .main_end(direction)
            .or(item.inset.main_start(direction).map(|pos| -pos))
            .unwrap_or(0.0)
    } else {
        item.inset
            .main_start(direction)
            .or(item.inset.main_end(direction).map(|pos| -pos))
            .unwrap_or(0.0)
    };
    let cross_relative_inset = if is_rtl_column {
        item.inset
            .cross_end(direction)
            .map(|pos| -pos)
            .or(item.inset.cross_start(direction))
            .unwrap_or(0.0)
    } else {
        item.inset
            .cross_start(direction)
            .or(item.inset.cross_end(direction).map(|pos| -pos))
            .unwrap_or(0.0)
    };
    let effective_line_offset_cross = if is_rtl_column {
        0.0
    } else {
        line_offset_cross
    };

    let offset_main = if is_rtl_row {
        *total_offset_main
            - item.offset_main
            - item.margin.main_end(direction)
            - main_relative_inset
            - size.width
    } else {
        *total_offset_main
            + item.offset_main
            + item.margin.main_start(direction)
            + main_relative_inset
    };
    let offset_cross = total_offset_cross
        + item.offset_cross
        + effective_line_offset_cross
        + item.margin.cross_start(direction)
        + cross_relative_inset;

    if direction.is_row() {
        let baseline_offset_cross = total_offset_cross
            + item.offset_cross
            + effective_line_offset_cross
            + item.margin.cross_start(direction);
        // A scroll container's baseline is clamped to its border box.
        let inner_baseline = {
            let baseline = layout_output.baselines.first.unwrap_or(size.height);
            if item.overflow.y.is_scroll_container() {
                baseline.min(size.height).max(0.0)
            } else {
                baseline
            }
        };
        item.baseline = baseline_offset_cross + inner_baseline;
    } else {
        let baseline_offset_main =
            *total_offset_main + item.offset_main + item.margin.main_start(direction);
        let inner_baseline = layout_output.baselines.first.unwrap_or(size.height);
        item.baseline = baseline_offset_main + inner_baseline;
    }

    let location = if direction.is_row() {
        Point {
            x: offset_main,
            y: offset_cross,
        }
    } else {
        Point {
            x: offset_cross,
            y: offset_main,
        }
    };
    let scrollbar_size = Size {
        width: if item.overflow.y == Overflow::Scroll {
            item.scrollbar_width
        } else {
            0.0
        },
        height: if item.overflow.x == Overflow::Scroll {
            item.scrollbar_width
        } else {
            0.0
        },
    };

    tree.set_layout(
        item.node,
        &Layout {
            order: item.order,
            size,
            scrollable_overflow_rect,
            scrollbar_size,
            location,
            padding: item.padding,
            border: item.border,
            margin: item.margin,
        },
    );

    if is_rtl_row {
        *total_offset_main -=
            item.offset_main + item.margin.main_axis_sum(direction) + size.main(direction);
    } else {
        *total_offset_main +=
            item.offset_main + item.margin.main_axis_sum(direction) + size.main(direction);
    }

    let contribution_location = if layout_direction == Direction::Rtl {
        Point {
            x: container_size.width - (location.x + size.width) - border.right,
            y: location.y - border.top,
        }
    } else {
        Point {
            x: location.x - border.left,
            y: location.y - border.top,
        }
    };
    *total_overflow_rect = total_overflow_rect.union(compute_scrollable_overflow_contribution(
        contribution_location,
        size,
        scrollable_overflow_rect,
        item.overflow,
        item.contain,
        constants.is_scroll_container,
    ));
}

/// Lays out the items of one line.
fn calculate_layout_line(
    tree: &mut impl LayoutTree,
    items: &mut [FlexItem],
    line: FlexLine,
    total_offset_cross: &mut f32,
    overflow_rect: &mut Rect<f32>,
    border: Rect<f32>,
    constants: &AlgoConstants,
) {
    let container_size = constants.container_size;
    let padding_border = constants.content_box_inset;
    let direction = constants.dir;
    let layout_direction = constants.layout_direction;
    let mut total_offset_main = if layout_direction == Direction::Rtl && direction.is_row() {
        container_size.width - padding_border.main_end(direction)
    } else {
        padding_border.main_start(direction)
    };
    let line_offset_cross = line.offset_cross;

    let is_rtl_column = layout_direction == Direction::Rtl && direction.is_column();
    if is_rtl_column {
        *total_offset_cross -= line_offset_cross + line.cross_size;
    }

    let line_items = &mut items[line.start..line.end];
    if direction.is_reverse() {
        for item in line_items.iter_mut().rev() {
            calculate_flex_item(
                tree,
                item,
                &mut total_offset_main,
                *total_offset_cross,
                line_offset_cross,
                overflow_rect,
                border,
                constants,
            );
        }
    } else {
        for item in line_items.iter_mut() {
            calculate_flex_item(
                tree,
                item,
                &mut total_offset_main,
                *total_offset_cross,
                line_offset_cross,
                overflow_rect,
                border,
                constants,
            );
        }
    }

    if !is_rtl_column {
        *total_offset_cross += line_offset_cross + line.cross_size;
    }
}

/// Lays out every line; returns the in-flow scrollable overflow.
fn final_layout_pass(
    tree: &mut impl LayoutTree,
    items: &mut [FlexItem],
    lines: &[FlexLine],
    constants: &AlgoConstants,
) -> Rect<f32> {
    let mut total_offset_cross =
        if constants.is_column && constants.layout_direction == Direction::Rtl {
            constants.container_size.width - constants.content_box_inset.cross_end(constants.dir)
        } else {
            constants.content_box_inset.cross_start(constants.dir)
        };
    let mut overflow_rect = Rect::ZERO;

    if constants.is_wrap_reverse {
        for line in lines.iter().rev() {
            calculate_layout_line(
                tree,
                items,
                *line,
                &mut total_offset_cross,
                &mut overflow_rect,
                constants.border,
                constants,
            );
        }
    } else {
        for line in lines.iter() {
            calculate_layout_line(
                tree,
                items,
                *line,
                &mut total_offset_cross,
                &mut overflow_rect,
                constants.border,
                constants,
            );
        }
    }

    // A scroll container's end padding is part of its scrollable
    // overflow.
    if constants.is_scroll_container {
        overflow_rect.right += if constants.layout_direction == Direction::Rtl {
            constants.content_box_inset.left - constants.border.left - constants.scrollbar_gutter.x
        } else {
            constants.content_box_inset.right
                - constants.border.right
                - constants.scrollbar_gutter.x
        };
        overflow_rect.bottom += constants.content_box_inset.bottom
            - constants.border.bottom
            - constants.scrollbar_gutter.y;
    }
    overflow_rect
}

/// Lays out the absolutely positioned children; returns their
/// scrollable overflow.
fn perform_absolute_layout_on_absolute_children(
    tree: &mut impl LayoutTree,
    node: Node,
    constants: &AlgoConstants,
) -> Rect<f32> {
    let dir = constants.dir;
    let container_width = constants.container_size.width;
    let container_height = constants.container_size.height;
    let inset_relative_size =
        constants.container_size - constants.border.sum_axes() - constants.scrollbar_gutter.into();
    let mut overflow_rect = Rect::ZERO;

    for order in 0..tree.child_count(node) {
        let child = tree.child(node, order);
        let child_style = *tree.row(child);
        if child_style.box_generation_mode() == BoxGenerationMode::None
            || child_style.position() != Position::Absolute
        {
            continue;
        }

        let overflow = child_style.overflow();
        let contain = child_style.contain();
        let scrollbar_width = child_style.scrollbar_width();
        let aspect_ratio = child_style.aspect_ratio();
        let align_self = resolve_self_relative(
            child_style.align_self().unwrap_or(constants.align_items),
            child_style.direction(),
            constants.layout_direction,
            constants.is_column,
        );
        let margin = child_style
            .margin()
            .map(|margin| margin.resolve_to_option(inset_relative_size.width, no_calc));
        let padding = child_style
            .padding()
            .resolve_or_zero(Some(inset_relative_size.width), no_calc);
        let border = child_style
            .border()
            .resolve_or_zero(Some(inset_relative_size.width), no_calc);
        let padding_border_sum = (padding + border).sum_axes();
        let box_sizing_adjustment = if child_style.box_sizing() == BoxSizing::ContentBox {
            padding_border_sum
        } else {
            Size::ZERO
        };

        // Insets resolve against the container minus its border.
        let inset = child_style.inset();
        let left = inset.left.maybe_resolve(inset_relative_size.width, no_calc);
        let right = inset
            .right
            .maybe_resolve(inset_relative_size.width, no_calc);
        let top = inset.top.maybe_resolve(inset_relative_size.height, no_calc);
        let bottom = inset
            .bottom
            .maybe_resolve(inset_relative_size.height, no_calc);

        // Known dimensions from the size, min, and max styles.
        let size_style = child_style.size();
        let style_size = size_style
            .maybe_resolve(inset_relative_size, no_calc)
            .maybe_apply_aspect_ratio(aspect_ratio)
            .maybe_add(box_sizing_adjustment);
        let min_size = child_style
            .min_size()
            .maybe_resolve(inset_relative_size, no_calc)
            .maybe_apply_aspect_ratio(aspect_ratio)
            .maybe_add(box_sizing_adjustment)
            .or(padding_border_sum.map(Some))
            .maybe_max(padding_border_sum);
        let max_size = child_style
            .max_size()
            .maybe_resolve(inset_relative_size, no_calc)
            .maybe_apply_aspect_ratio(aspect_ratio)
            .maybe_add(box_sizing_adjustment);
        let mut known_dimensions = style_size.maybe_clamp(min_size, max_size);

        // Keyword sizes; a set axis wins over the insets below.
        if size_style.width.is_sizing_keyword() || size_style.height.is_sizing_keyword() {
            resolve_absolute_sizing_keywords(
                tree,
                child,
                &mut known_dimensions,
                size_style,
                inset_relative_size,
                Rect {
                    left,
                    right,
                    top,
                    bottom,
                },
                margin,
                SizingMode::ContentSize,
            );
            known_dimensions = known_dimensions
                .maybe_apply_aspect_ratio(aspect_ratio)
                .maybe_clamp(min_size, max_size);
        }

        // Width from left and right, if unknown.
        if let (None, Some(left), Some(right)) = (known_dimensions.width, left, right) {
            let new_width_raw = inset_relative_size
                .width
                .maybe_sub(margin.left)
                .maybe_sub(margin.right)
                - left
                - right;
            known_dimensions.width = Some(new_width_raw.max(0.0));
            known_dimensions = known_dimensions
                .maybe_apply_aspect_ratio(aspect_ratio)
                .maybe_clamp(min_size, max_size);
        }

        // Height from top and bottom, if unknown.
        if let (None, Some(top), Some(bottom)) = (known_dimensions.height, top, bottom) {
            let new_height_raw = inset_relative_size
                .height
                .maybe_sub(margin.top)
                .maybe_sub(margin.bottom)
                - top
                - bottom;
            known_dimensions.height = Some(new_height_raw.max(0.0));
            known_dimensions = known_dimensions
                .maybe_apply_aspect_ratio(aspect_ratio)
                .maybe_clamp(min_size, max_size);
        }
        let final_size = match (known_dimensions.width, known_dimensions.height) {
            (Some(width), Some(height)) => Size { width, height },
            _ => {
                let measured_size = measure_child_size_both(
                    tree,
                    child,
                    known_dimensions,
                    constants.node_inner_size,
                    Size {
                        width: AvailableSpace::Definite(
                            container_width.maybe_clamp(min_size.width, max_size.width),
                        ),
                        height: AvailableSpace::Definite(
                            container_height.maybe_clamp(min_size.height, max_size.height),
                        ),
                    },
                    SizingMode::ContentSize,
                );
                known_dimensions.unwrap_or(measured_size)
            }
        }
        .maybe_clamp(min_size, max_size);

        let layout_output = perform_child_layout(
            tree,
            child,
            final_size.map(Some),
            constants.node_inner_size,
            Size {
                width: AvailableSpace::Definite(
                    container_width.maybe_clamp(min_size.width, max_size.width),
                ),
                height: AvailableSpace::Definite(
                    container_height.maybe_clamp(min_size.height, max_size.height),
                ),
            },
            SizingMode::ContentSize,
        );

        let non_auto_margin = margin.map(|m| m.unwrap_or(0.0));
        let free_space = Size {
            width: constants.container_size.width
                - final_size.width
                - non_auto_margin.horizontal_axis_sum(),
            height: constants.container_size.height
                - final_size.height
                - non_auto_margin.vertical_axis_sum(),
        }
        .f32_max(Size::ZERO);

        // Auto margins absorb free space only between two set insets;
        // otherwise they are zero (CSS2 10.3.7, 10.6.4).
        let resolved_margin = {
            let auto_margin_size = Size {
                width: {
                    let auto_margin_count =
                        margin.left.is_none() as u8 + margin.right.is_none() as u8;
                    if auto_margin_count > 0 && left.is_some() && right.is_some() {
                        free_space.width / auto_margin_count as f32
                    } else {
                        0.0
                    }
                },
                height: {
                    let auto_margin_count =
                        margin.top.is_none() as u8 + margin.bottom.is_none() as u8;
                    if auto_margin_count > 0 && top.is_some() && bottom.is_some() {
                        free_space.height / auto_margin_count as f32
                    } else {
                        0.0
                    }
                },
            };
            Rect {
                left: margin.left.unwrap_or(auto_margin_size.width),
                right: margin.right.unwrap_or(auto_margin_size.width),
                top: margin.top.unwrap_or(auto_margin_size.height),
                bottom: margin.bottom.unwrap_or(auto_margin_size.height),
            }
        };

        // Flex-relative insets.
        let (start_main, end_main) = if constants.is_row {
            (left, right)
        } else {
            (top, bottom)
        };
        let (start_cross, end_cross) = if constants.is_row {
            (top, bottom)
        } else {
            (left, right)
        };
        let main_axis_is_horizontal = constants.is_row;
        let cross_axis_is_horizontal = !constants.is_row;
        let main_is_rtl = main_axis_is_horizontal && constants.layout_direction == Direction::Rtl;
        let cross_is_rtl = cross_axis_is_horizontal && constants.layout_direction == Direction::Rtl;
        let main_axis_flex_start_reversed = dir.is_reverse() ^ main_is_rtl;
        let cross_axis_flex_start_reversed = constants.is_wrap_reverse ^ cross_is_rtl;
        let main_start_scrollbar_offset = if main_is_rtl {
            constants.scrollbar_gutter.main(dir)
        } else {
            0.0
        };
        let cross_start_scrollbar_offset = if cross_is_rtl {
            constants.scrollbar_gutter.cross(dir)
        } else {
            0.0
        };
        let main_end_scrollbar_offset = if main_is_rtl {
            0.0
        } else {
            constants.scrollbar_gutter.main(dir)
        };
        let cross_end_scrollbar_offset = if cross_is_rtl {
            0.0
        } else {
            constants.scrollbar_gutter.cross(dir)
        };

        // Main axis.
        let offset_main = if start_main.is_some() || end_main.is_some() {
            if main_is_rtl && end_main.is_some() {
                constants.container_size.main(dir)
                    - constants.border.main_end(dir)
                    - main_end_scrollbar_offset
                    - final_size.main(dir)
                    - end_main.unwrap_or(0.0)
                    - resolved_margin.main_end(dir)
            } else if let Some(start) = start_main {
                start
                    + constants.border.main_start(dir)
                    + main_start_scrollbar_offset
                    + resolved_margin.main_start(dir)
            } else {
                constants.container_size.main(dir)
                    - constants.border.main_end(dir)
                    - main_end_scrollbar_offset
                    - final_size.main(dir)
                    - end_main.unwrap_or(0.0)
                    - resolved_margin.main_end(dir)
            }
        } else {
            // `stretch` is invalid for justify-content in flexbox: it acts
            // as flex-start. As in Chrome, `safe` does not apply here.
            // `start` and `end` flip for RTL only; the flex keywords and
            // the distributed fallbacks are flex-relative.
            let keyword = constants
                .justify_content
                .unwrap_or(JustifyContent::FLEX_START)
                .keyword();
            let start_position = match keyword {
                AlignContentKeyword::Start => !main_is_rtl,
                AlignContentKeyword::End => main_is_rtl,
                _ => true,
            };
            match (keyword, main_axis_flex_start_reversed) {
                (AlignContentKeyword::SpaceBetween, false)
                | (AlignContentKeyword::Stretch, false)
                | (AlignContentKeyword::FlexStart, false)
                | (AlignContentKeyword::FlexEnd, true) => {
                    constants.content_box_inset.main_start(dir) + resolved_margin.main_start(dir)
                }
                (AlignContentKeyword::Start | AlignContentKeyword::End, _) => {
                    if start_position {
                        constants.content_box_inset.main_start(dir)
                            + resolved_margin.main_start(dir)
                    } else {
                        constants.container_size.main(dir)
                            - constants.content_box_inset.main_end(dir)
                            - final_size.main(dir)
                            - resolved_margin.main_end(dir)
                    }
                }
                (AlignContentKeyword::FlexEnd, false)
                | (AlignContentKeyword::FlexStart, true)
                | (AlignContentKeyword::Stretch, true)
                | (AlignContentKeyword::SpaceBetween, true) => {
                    constants.container_size.main(dir)
                        - constants.content_box_inset.main_end(dir)
                        - final_size.main(dir)
                        - resolved_margin.main_end(dir)
                }
                (AlignContentKeyword::SpaceEvenly, _)
                | (AlignContentKeyword::SpaceAround, _)
                | (AlignContentKeyword::Center, _) => {
                    (constants.container_size.main(dir)
                        + constants.content_box_inset.main_start(dir)
                        - constants.content_box_inset.main_end(dir)
                        - final_size.main(dir)
                        + resolved_margin.main_start(dir)
                        - resolved_margin.main_end(dir))
                        / 2.0
                }
            }
        };

        // Cross axis.
        let offset_cross = if start_cross.is_some() || end_cross.is_some() {
            if cross_is_rtl && end_cross.is_some() {
                constants.container_size.cross(dir)
                    - constants.border.cross_end(dir)
                    - cross_end_scrollbar_offset
                    - final_size.cross(dir)
                    - end_cross.unwrap_or(0.0)
                    - resolved_margin.cross_end(dir)
            } else if let Some(start) = start_cross {
                start
                    + constants.border.cross_start(dir)
                    + cross_start_scrollbar_offset
                    + resolved_margin.cross_start(dir)
            } else {
                constants.container_size.cross(dir)
                    - constants.border.cross_end(dir)
                    - cross_end_scrollbar_offset
                    - final_size.cross(dir)
                    - end_cross.unwrap_or(0.0)
                    - resolved_margin.cross_end(dir)
            }
        } else {
            let cross_overflows = final_size.cross(dir) + resolved_margin.cross_axis_sum(dir)
                > constants.container_size.cross(dir)
                    - constants.content_box_inset.cross_axis_sum(dir);
            let cross_keyword = resolve_self_alignment_safety(align_self, cross_overflows);
            // `start`, `end`, and `baseline` (static fallback: start) flip
            // for RTL only; flex-start, flex-end, and stretch are
            // flex-relative.
            let start_position = match cross_keyword {
                AlignItemsKeyword::Start | AlignItemsKeyword::Baseline => !cross_is_rtl,
                AlignItemsKeyword::End => cross_is_rtl,
                _ => true,
            };
            match (cross_keyword, cross_axis_flex_start_reversed) {
                // Stretch does not apply to absolute items.
                (
                    AlignItemsKeyword::Start | AlignItemsKeyword::End | AlignItemsKeyword::Baseline,
                    _,
                ) => {
                    if start_position {
                        constants.content_box_inset.cross_start(dir)
                            + resolved_margin.cross_start(dir)
                    } else {
                        constants.container_size.cross(dir)
                            - constants.content_box_inset.cross_end(dir)
                            - final_size.cross(dir)
                            - resolved_margin.cross_end(dir)
                    }
                }
                (AlignItemsKeyword::Stretch | AlignItemsKeyword::FlexStart, false)
                | (AlignItemsKeyword::FlexEnd, true) => {
                    constants.content_box_inset.cross_start(dir) + resolved_margin.cross_start(dir)
                }
                (AlignItemsKeyword::Stretch | AlignItemsKeyword::FlexStart, true)
                | (AlignItemsKeyword::FlexEnd, false) => {
                    constants.container_size.cross(dir)
                        - constants.content_box_inset.cross_end(dir)
                        - final_size.cross(dir)
                        - resolved_margin.cross_end(dir)
                }
                (AlignItemsKeyword::Center, _) => {
                    (constants.container_size.cross(dir)
                        + constants.content_box_inset.cross_start(dir)
                        - constants.content_box_inset.cross_end(dir)
                        - final_size.cross(dir)
                        + resolved_margin.cross_start(dir)
                        - resolved_margin.cross_end(dir))
                        / 2.0
                }
                (AlignItemsKeyword::SelfStart | AlignItemsKeyword::SelfEnd, _) => unreachable!(),
            }
        };

        let location = if constants.is_row {
            Point {
                x: offset_main,
                y: offset_cross,
            }
        } else {
            Point {
                x: offset_cross,
                y: offset_main,
            }
        };
        let scrollbar_size = Size {
            width: if overflow.y == Overflow::Scroll {
                scrollbar_width
            } else {
                0.0
            },
            height: if overflow.x == Overflow::Scroll {
                scrollbar_width
            } else {
                0.0
            },
        };
        tree.set_layout(
            child,
            &Layout {
                order: order as u32,
                size: final_size,
                scrollable_overflow_rect: layout_output.scrollable_overflow_rect,
                scrollbar_size,
                location,
                padding,
                border,
                margin: resolved_margin,
            },
        );

        // The location from the scroll origin (the right edge in RTL).
        let absolute_area_offset = Point {
            x: constants.border.left
                + if constants.layout_direction == Direction::Rtl {
                    constants.scrollbar_gutter.x
                } else {
                    0.0
                },
            y: constants.border.top,
        };
        let relative_location = Point {
            x: location.x - absolute_area_offset.x,
            y: location.y - absolute_area_offset.y,
        };
        let contribution_location = if constants.layout_direction == Direction::Rtl {
            Point {
                x: inset_relative_size.width - relative_location.x - final_size.width,
                y: relative_location.y,
            }
        } else {
            relative_location
        };
        overflow_rect = overflow_rect.union(compute_scrollable_overflow_contribution(
            contribution_location,
            final_size,
            layout_output.scrollable_overflow_rect,
            overflow,
            contain,
            constants.is_scroll_container,
        ));
    }
    overflow_rect
}

/// The gaps between `num_items` items.
#[inline(always)]
fn sum_axis_gaps(gap: f32, num_items: usize) -> f32 {
    if num_items <= 1 {
        0.0
    } else {
        gap * (num_items - 1) as f32
    }
}
