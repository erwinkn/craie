use taffy::{
    AlignContent, AlignContentKeyword, AlignItems, AlignItemsKeyword, AlignmentSafety,
    BoxGenerationMode, BoxSizing, Contain, CoreStyle, Dimension, Direction, Display,
    ExpandedDimension, ExpandedLengthPercentage, ExpandedLengthPercentageAuto, FlexDirection,
    FlexWrap, FlexboxContainerStyle, FlexboxItemStyle, JustifyContent, LengthPercentage,
    LengthPercentageAuto, Overflow, Point, Position, Rect, Size, Style,
};

const SLOT_COUNT: usize = 25;

const INSET_L: usize = 0;
const INSET_R: usize = 1;
const INSET_T: usize = 2;
const INSET_B: usize = 3;
const SIZE_W: usize = 4;
const SIZE_H: usize = 5;
const MIN_W: usize = 6;
const MIN_H: usize = 7;
const MAX_W: usize = 8;
const MAX_H: usize = 9;
const MARGIN_L: usize = 10;
const MARGIN_R: usize = 11;
const MARGIN_T: usize = 12;
const MARGIN_B: usize = 13;
const PADDING_L: usize = 14;
const PADDING_R: usize = 15;
const PADDING_T: usize = 16;
const PADDING_B: usize = 17;
const BORDER_L: usize = 18;
const BORDER_R: usize = 19;
const BORDER_T: usize = 20;
const BORDER_B: usize = 21;
const GAP_W: usize = 22;
const GAP_H: usize = 23;
const FLEX_BASIS: usize = 24;

const TAG_AUTO: u64 = 0;
const TAG_LENGTH: u64 = 1;
const TAG_PERCENT: u64 = 2;
const TAG_SPECIAL: u64 = 3;

const TAG_UNSET: u8 = 0xff;
const TAG_SAFE: u8 = 0x80;

const FLAG_DISPLAY_NONE: u8 = 1 << 0;
const FLAG_ABSOLUTE: u8 = 1 << 1;
const FLAG_CONTENT_BOX: u8 = 1 << 2;
const FLAG_RTL: u8 = 1 << 3;
const FLAG_ASPECT: u8 = 1 << 4;
const FLAG_CONTAIN_LAYOUT: u8 = 1 << 5;
const FLAG_CONTAIN_PAINT: u8 = 1 << 6;
const DIM_MIN_CONTENT: u32 = 0x7fc0_0001;
const DIM_MAX_CONTENT: u32 = 0x7fc0_0002;
const DIM_FIT_CONTENT: u32 = 0x7fc0_0003;
const DIM_STRETCH: u32 = 0x7fc0_0004;
const DIM_CONTENT: u32 = 0x7fc0_0005;

/// The layout fields that Craie stores per node.
///
/// The row keeps every `taffy::Style` field that the flexbox build of
/// Taffy reads. It drops `item_is_table` and `item_is_replaced`, because
/// only the block and grid algorithms read them, and Craie does not
/// build those algorithms.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LayoutRow {
    tags: u64,
    values: [f32; SLOT_COUNT],
    flex_grow: f32,
    flex_shrink: f32,
    aspect_ratio: f32,
    scrollbar_width: f32,
    flags: u8,
    flex_direction: u8,
    flex_wrap: u8,
    justify_content: u8,
    align_items: u8,
    align_content: u8,
    align_self: u8,
    overflow_x: u8,
    overflow_y: u8,
    dim_extra: [u8; 3],
}

impl PartialEq for LayoutRow {
    fn eq(&self, other: &Self) -> bool {
        self.tags == other.tags
            && self
                .values
                .iter()
                .zip(other.values)
                .all(|(a, b)| a.to_bits() == b.to_bits())
            && self.flex_grow.to_bits() == other.flex_grow.to_bits()
            && self.flex_shrink.to_bits() == other.flex_shrink.to_bits()
            && self.aspect_ratio.to_bits() == other.aspect_ratio.to_bits()
            && self.scrollbar_width.to_bits() == other.scrollbar_width.to_bits()
            && self.flags == other.flags
            && self.flex_direction == other.flex_direction
            && self.flex_wrap == other.flex_wrap
            && self.justify_content == other.justify_content
            && self.align_items == other.align_items
            && self.align_content == other.align_content
            && self.align_self == other.align_self
            && self.overflow_x == other.overflow_x
            && self.overflow_y == other.overflow_y
            && self.dim_extra == other.dim_extra
    }
}

impl Default for LayoutRow {
    fn default() -> LayoutRow {
        LayoutRow::from(&Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            ..Style::default()
        })
    }
}

impl From<&Style> for LayoutRow {
    fn from(style: &Style) -> LayoutRow {
        let mut row = LayoutRow {
            tags: 0,
            values: [0.0; SLOT_COUNT],
            flex_grow: style.flex_grow,
            flex_shrink: style.flex_shrink,
            aspect_ratio: style.aspect_ratio.unwrap_or(0.0),
            scrollbar_width: style.scrollbar_width,
            flags: flags_of(style),
            flex_direction: flex_direction_tag(style.flex_direction),
            flex_wrap: flex_wrap_tag(style.flex_wrap),
            justify_content: content_tag(style.justify_content),
            align_items: items_tag(style.align_items),
            align_content: content_tag(style.align_content),
            align_self: items_tag(style.align_self),
            overflow_x: overflow_tag(style.overflow.x),
            overflow_y: overflow_tag(style.overflow.y),
            dim_extra: [0; 3],
        };
        row.set_lpa_slot(INSET_L, style.inset.left);
        row.set_lpa_slot(INSET_R, style.inset.right);
        row.set_lpa_slot(INSET_T, style.inset.top);
        row.set_lpa_slot(INSET_B, style.inset.bottom);
        row.set_dim_slot(SIZE_W, style.size.width);
        row.set_dim_slot(SIZE_H, style.size.height);
        row.set_lpa_slot(MIN_W, style.min_size.width);
        row.set_lpa_slot(MIN_H, style.min_size.height);
        row.set_lpa_slot(MAX_W, style.max_size.width);
        row.set_lpa_slot(MAX_H, style.max_size.height);
        row.set_lpa_slot(MARGIN_L, style.margin.left);
        row.set_lpa_slot(MARGIN_R, style.margin.right);
        row.set_lpa_slot(MARGIN_T, style.margin.top);
        row.set_lpa_slot(MARGIN_B, style.margin.bottom);
        row.set_lp_slot(PADDING_L, style.padding.left);
        row.set_lp_slot(PADDING_R, style.padding.right);
        row.set_lp_slot(PADDING_T, style.padding.top);
        row.set_lp_slot(PADDING_B, style.padding.bottom);
        row.set_lp_slot(BORDER_L, style.border.left);
        row.set_lp_slot(BORDER_R, style.border.right);
        row.set_lp_slot(BORDER_T, style.border.top);
        row.set_lp_slot(BORDER_B, style.border.bottom);
        row.set_lp_slot(GAP_W, style.gap.width);
        row.set_lp_slot(GAP_H, style.gap.height);
        row.set_dim_slot(FLEX_BASIS, style.flex_basis);
        row
    }
}

impl LayoutRow {
    pub fn to_taffy(&self) -> Style {
        Style {
            display: self.display(),
            box_sizing: self.box_sizing(),
            direction: self.direction(),
            overflow: self.overflow(),
            scrollbar_width: self.scrollbar_width(),
            contain: self.contain(),
            position: self.position(),
            inset: self.inset(),
            size: self.size(),
            min_size: self.min_size(),
            max_size: self.max_size(),
            aspect_ratio: self.aspect_ratio(),
            margin: self.margin(),
            padding: self.padding(),
            border: self.border(),
            align_items: self.align_items(),
            align_self: self.align_self(),
            align_content: self.align_content(),
            justify_content: self.justify_content(),
            gap: self.gap(),
            flex_direction: self.flex_direction(),
            flex_wrap: self.flex_wrap(),
            flex_grow: self.flex_grow,
            flex_shrink: self.flex_shrink,
            flex_basis: self.flex_basis(),
            ..Style::default()
        }
    }

    pub fn display(&self) -> Display {
        match self.flag(FLAG_DISPLAY_NONE) {
            true => Display::None,
            false => Display::Flex,
        }
    }

    pub fn position(&self) -> Position {
        match self.flag(FLAG_ABSOLUTE) {
            true => Position::Absolute,
            false => Position::Relative,
        }
    }

    pub fn direction(&self) -> Direction {
        match self.flag(FLAG_RTL) {
            true => Direction::Rtl,
            false => Direction::Ltr,
        }
    }

    pub fn scrollbar_width(&self) -> f32 {
        self.scrollbar_width
    }

    pub fn contain(&self) -> Contain {
        let mut contain = Contain::NONE;
        if self.flag(FLAG_CONTAIN_LAYOUT) {
            contain |= Contain::LAYOUT;
        }
        if self.flag(FLAG_CONTAIN_PAINT) {
            contain |= Contain::PAINT;
        }
        contain
    }

    pub fn overflow(&self) -> Point<Overflow> {
        Point {
            x: overflow_of(self.overflow_x),
            y: overflow_of(self.overflow_y),
        }
    }

    pub fn size(&self) -> Size<Dimension> {
        Size {
            width: self.dim_slot(SIZE_W),
            height: self.dim_slot(SIZE_H),
        }
    }

    pub fn min_size(&self) -> Size<LengthPercentageAuto> {
        Size {
            width: self.lpa_slot(MIN_W),
            height: self.lpa_slot(MIN_H),
        }
    }

    pub fn max_size(&self) -> Size<LengthPercentageAuto> {
        Size {
            width: self.lpa_slot(MAX_W),
            height: self.lpa_slot(MAX_H),
        }
    }

    pub fn margin(&self) -> Rect<LengthPercentageAuto> {
        Rect {
            left: self.lpa_slot(MARGIN_L),
            right: self.lpa_slot(MARGIN_R),
            top: self.lpa_slot(MARGIN_T),
            bottom: self.lpa_slot(MARGIN_B),
        }
    }

    pub fn padding(&self) -> Rect<LengthPercentage> {
        Rect {
            left: self.lp_slot(PADDING_L),
            right: self.lp_slot(PADDING_R),
            top: self.lp_slot(PADDING_T),
            bottom: self.lp_slot(PADDING_B),
        }
    }

    pub fn border(&self) -> Rect<LengthPercentage> {
        Rect {
            left: self.lp_slot(BORDER_L),
            right: self.lp_slot(BORDER_R),
            top: self.lp_slot(BORDER_T),
            bottom: self.lp_slot(BORDER_B),
        }
    }

    pub fn inset(&self) -> Rect<LengthPercentageAuto> {
        Rect {
            left: self.lpa_slot(INSET_L),
            right: self.lpa_slot(INSET_R),
            top: self.lpa_slot(INSET_T),
            bottom: self.lpa_slot(INSET_B),
        }
    }

    pub fn gap(&self) -> Size<LengthPercentage> {
        Size {
            width: self.lp_slot(GAP_W),
            height: self.lp_slot(GAP_H),
        }
    }

    pub fn flex_basis(&self) -> Dimension {
        self.dim_slot(FLEX_BASIS)
    }

    pub fn flex_grow(&self) -> f32 {
        self.flex_grow
    }

    pub fn flex_shrink(&self) -> f32 {
        self.flex_shrink
    }

    pub fn flex_direction(&self) -> FlexDirection {
        match self.flex_direction {
            1 => FlexDirection::Column,
            2 => FlexDirection::RowReverse,
            3 => FlexDirection::ColumnReverse,
            _ => FlexDirection::Row,
        }
    }

    pub fn flex_wrap(&self) -> FlexWrap {
        match self.flex_wrap {
            1 => FlexWrap::Wrap,
            2 => FlexWrap::WrapReverse,
            _ => FlexWrap::NoWrap,
        }
    }

    pub fn justify_content(&self) -> Option<JustifyContent> {
        content_of(self.justify_content)
    }

    pub fn align_items(&self) -> Option<AlignItems> {
        items_of(self.align_items)
    }

    pub fn align_content(&self) -> Option<AlignContent> {
        content_of(self.align_content)
    }

    pub fn align_self(&self) -> Option<taffy::AlignSelf> {
        items_of(self.align_self)
    }

    pub fn aspect_ratio(&self) -> Option<f32> {
        self.flag(FLAG_ASPECT).then_some(self.aspect_ratio)
    }

    pub fn box_sizing(&self) -> BoxSizing {
        match self.flag(FLAG_CONTENT_BOX) {
            true => BoxSizing::ContentBox,
            false => BoxSizing::BorderBox,
        }
    }

    pub fn set_width(&mut self, value: Dimension) {
        self.set_dim_slot(SIZE_W, value);
    }

    pub fn set_height(&mut self, value: Dimension) {
        self.set_dim_slot(SIZE_H, value);
    }

    pub fn set_padding(&mut self, value: Rect<LengthPercentage>) {
        self.set_lp_slot(PADDING_L, value.left);
        self.set_lp_slot(PADDING_R, value.right);
        self.set_lp_slot(PADDING_T, value.top);
        self.set_lp_slot(PADDING_B, value.bottom);
    }

    pub fn set_border(&mut self, value: Rect<LengthPercentage>) {
        self.set_lp_slot(BORDER_L, value.left);
        self.set_lp_slot(BORDER_R, value.right);
        self.set_lp_slot(BORDER_T, value.top);
        self.set_lp_slot(BORDER_B, value.bottom);
    }

    pub fn set_gap(&mut self, value: Size<LengthPercentage>) {
        self.set_lp_slot(GAP_W, value.width);
        self.set_lp_slot(GAP_H, value.height);
    }

    fn flag(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    fn tag(&self, slot: usize) -> u64 {
        (self.tags >> (slot * 2)) & 0b11
    }

    fn set_tag(&mut self, slot: usize, tag: u64) {
        let shift = slot * 2;
        self.tags = (self.tags & !(0b11 << shift)) | (tag << shift);
    }

    fn set_lp_slot(&mut self, slot: usize, value: LengthPercentage) {
        match value.expand() {
            ExpandedLengthPercentage::Length(v) => {
                self.values[slot] = v;
                self.set_tag(slot, TAG_LENGTH);
            }
            ExpandedLengthPercentage::Percent(v) => {
                self.values[slot] = v;
                self.set_tag(slot, TAG_PERCENT);
            }
        }
    }

    fn lp_slot(&self, slot: usize) -> LengthPercentage {
        match self.tag(slot) {
            TAG_PERCENT => LengthPercentage::percent(self.values[slot]),
            _ => LengthPercentage::length(self.values[slot]),
        }
    }

    fn set_lpa_slot(&mut self, slot: usize, value: LengthPercentageAuto) {
        match value.expand() {
            ExpandedLengthPercentageAuto::Length(v) => {
                self.values[slot] = v;
                self.set_tag(slot, TAG_LENGTH);
            }
            ExpandedLengthPercentageAuto::Percent(v) => {
                self.values[slot] = v;
                self.set_tag(slot, TAG_PERCENT);
            }
            ExpandedLengthPercentageAuto::Auto => {
                self.values[slot] = 0.0;
                self.set_tag(slot, TAG_AUTO);
            }
        }
    }

    fn lpa_slot(&self, slot: usize) -> LengthPercentageAuto {
        match self.tag(slot) {
            TAG_LENGTH => LengthPercentageAuto::length(self.values[slot]),
            TAG_PERCENT => LengthPercentageAuto::percent(self.values[slot]),
            _ => LengthPercentageAuto::auto(),
        }
    }

    fn set_dim_slot(&mut self, slot: usize, value: Dimension) {
        self.dim_extra[dim_extra_index(slot)] = 0;
        match value.expand() {
            ExpandedDimension::Length(v) => {
                self.values[slot] = v;
                self.set_tag(slot, TAG_LENGTH);
            }
            ExpandedDimension::Percent(v) => {
                self.values[slot] = v;
                self.set_tag(slot, TAG_PERCENT);
            }
            ExpandedDimension::Auto => {
                self.values[slot] = 0.0;
                self.set_tag(slot, TAG_AUTO);
            }
            ExpandedDimension::MinContent => self.set_dim_keyword(slot, DIM_MIN_CONTENT),
            ExpandedDimension::MaxContent => self.set_dim_keyword(slot, DIM_MAX_CONTENT),
            ExpandedDimension::FitContent => self.set_dim_keyword(slot, DIM_FIT_CONTENT),
            ExpandedDimension::Stretch => self.set_dim_keyword(slot, DIM_STRETCH),
            ExpandedDimension::Content => self.set_dim_keyword(slot, DIM_CONTENT),
            ExpandedDimension::FitContentPx(v) => {
                self.values[slot] = v;
                self.dim_extra[dim_extra_index(slot)] = 1;
                self.set_tag(slot, TAG_SPECIAL);
            }
            ExpandedDimension::FitContentPercent(v) => {
                self.values[slot] = v;
                self.dim_extra[dim_extra_index(slot)] = 2;
                self.set_tag(slot, TAG_SPECIAL);
            }
        }
    }

    fn set_dim_keyword(&mut self, slot: usize, bits: u32) {
        self.dim_extra[dim_extra_index(slot)] = 0;
        self.values[slot] = f32::from_bits(bits);
        self.set_tag(slot, TAG_SPECIAL);
    }

    fn dim_slot(&self, slot: usize) -> Dimension {
        match self.tag(slot) {
            TAG_LENGTH => Dimension::length(self.values[slot]),
            TAG_PERCENT => Dimension::percent(self.values[slot]),
            TAG_SPECIAL => match self.dim_extra[dim_extra_index(slot)] {
                1 => Dimension::fit_content_px(self.values[slot]),
                2 => Dimension::fit_content_percent(self.values[slot]),
                _ => match self.values[slot].to_bits() {
                    DIM_MIN_CONTENT => Dimension::min_content(),
                    DIM_MAX_CONTENT => Dimension::max_content(),
                    DIM_FIT_CONTENT => Dimension::fit_content(),
                    DIM_STRETCH => Dimension::stretch(),
                    DIM_CONTENT => Dimension::content(),
                    _ => Dimension::auto(),
                },
            },
            _ => Dimension::auto(),
        }
    }
}

fn dim_extra_index(slot: usize) -> usize {
    match slot {
        SIZE_W => 0,
        SIZE_H => 1,
        FLEX_BASIS => 2,
        _ => unreachable!("dimension slot"),
    }
}

impl CoreStyle for LayoutRow {
    type CustomIdent = String;

    #[inline(always)]
    fn box_generation_mode(&self) -> BoxGenerationMode {
        match self.display() {
            Display::None => BoxGenerationMode::None,
            _ => BoxGenerationMode::Normal,
        }
    }

    #[inline(always)]
    fn is_compressible_replaced(&self) -> bool {
        false
    }

    #[inline(always)]
    fn box_sizing(&self) -> BoxSizing {
        self.box_sizing()
    }

    #[inline(always)]
    fn direction(&self) -> Direction {
        self.direction()
    }

    #[inline(always)]
    fn overflow(&self) -> Point<Overflow> {
        self.overflow()
    }

    #[inline(always)]
    fn scrollbar_width(&self) -> f32 {
        self.scrollbar_width()
    }

    #[inline(always)]
    fn position(&self) -> Position {
        self.position()
    }

    #[inline(always)]
    fn inset(&self) -> Rect<LengthPercentageAuto> {
        self.inset()
    }

    #[inline(always)]
    fn size(&self) -> Size<Dimension> {
        self.size()
    }

    #[inline(always)]
    fn min_size(&self) -> Size<LengthPercentageAuto> {
        self.min_size()
    }

    #[inline(always)]
    fn max_size(&self) -> Size<LengthPercentageAuto> {
        self.max_size()
    }

    #[inline(always)]
    fn aspect_ratio(&self) -> Option<f32> {
        self.aspect_ratio()
    }

    #[inline(always)]
    fn margin(&self) -> Rect<LengthPercentageAuto> {
        self.margin()
    }

    #[inline(always)]
    fn padding(&self) -> Rect<LengthPercentage> {
        self.padding()
    }

    #[inline(always)]
    fn border(&self) -> Rect<LengthPercentage> {
        self.border()
    }

    #[inline(always)]
    fn contain(&self) -> Contain {
        self.contain()
    }
}

impl FlexboxContainerStyle for LayoutRow {
    #[inline(always)]
    fn flex_direction(&self) -> FlexDirection {
        self.flex_direction()
    }

    #[inline(always)]
    fn flex_wrap(&self) -> FlexWrap {
        self.flex_wrap()
    }

    #[inline(always)]
    fn gap(&self) -> Size<LengthPercentage> {
        self.gap()
    }

    #[inline(always)]
    fn align_content(&self) -> Option<AlignContent> {
        self.align_content()
    }

    #[inline(always)]
    fn align_items(&self) -> Option<AlignItems> {
        self.align_items()
    }

    #[inline(always)]
    fn justify_content(&self) -> Option<JustifyContent> {
        self.justify_content()
    }
}

impl FlexboxItemStyle for LayoutRow {
    #[inline(always)]
    fn flex_basis(&self) -> Dimension {
        self.flex_basis()
    }

    #[inline(always)]
    fn flex_grow(&self) -> f32 {
        self.flex_grow
    }

    #[inline(always)]
    fn flex_shrink(&self) -> f32 {
        self.flex_shrink
    }

    #[inline(always)]
    fn align_self(&self) -> Option<taffy::AlignSelf> {
        self.align_self()
    }
}

fn flags_of(style: &Style) -> u8 {
    let mut flags = 0;
    let mut set = |flag, on| {
        if on {
            flags |= flag;
        }
    };
    set(FLAG_DISPLAY_NONE, style.display == Display::None);
    set(FLAG_ABSOLUTE, style.position == Position::Absolute);
    set(FLAG_CONTENT_BOX, style.box_sizing == BoxSizing::ContentBox);
    set(FLAG_RTL, style.direction == Direction::Rtl);
    set(FLAG_ASPECT, style.aspect_ratio.is_some());
    set(FLAG_CONTAIN_LAYOUT, style.contain.contains(Contain::LAYOUT));
    set(FLAG_CONTAIN_PAINT, style.contain.contains(Contain::PAINT));
    flags
}

fn flex_direction_tag(value: FlexDirection) -> u8 {
    match value {
        FlexDirection::Row => 0,
        FlexDirection::Column => 1,
        FlexDirection::RowReverse => 2,
        FlexDirection::ColumnReverse => 3,
    }
}

fn flex_wrap_tag(value: FlexWrap) -> u8 {
    match value {
        FlexWrap::NoWrap => 0,
        FlexWrap::Wrap => 1,
        FlexWrap::WrapReverse => 2,
        #[allow(unreachable_patterns)]
        _ => 0,
    }
}

fn overflow_tag(value: Overflow) -> u8 {
    match value {
        Overflow::Visible => 0,
        Overflow::Clip => 1,
        Overflow::Hidden => 2,
        Overflow::Scroll => 3,
    }
}

fn overflow_of(tag: u8) -> Overflow {
    match tag {
        1 => Overflow::Clip,
        2 => Overflow::Hidden,
        3 => Overflow::Scroll,
        _ => Overflow::Visible,
    }
}

fn safety_tag(value: AlignmentSafety) -> u8 {
    match value {
        AlignmentSafety::Unsafe => 0,
        AlignmentSafety::Safe => TAG_SAFE,
    }
}

fn safety_of(tag: u8) -> AlignmentSafety {
    match tag & TAG_SAFE {
        0 => AlignmentSafety::Unsafe,
        _ => AlignmentSafety::Safe,
    }
}

fn items_tag(value: Option<AlignItems>) -> u8 {
    let Some(value) = value else {
        return TAG_UNSET;
    };
    safety_tag(value.safety)
        | match value.keyword {
            AlignItemsKeyword::Start => 0,
            AlignItemsKeyword::End => 1,
            AlignItemsKeyword::FlexStart => 2,
            AlignItemsKeyword::FlexEnd => 3,
            AlignItemsKeyword::SelfStart => 4,
            AlignItemsKeyword::SelfEnd => 5,
            AlignItemsKeyword::Center => 6,
            AlignItemsKeyword::Baseline => 7,
            AlignItemsKeyword::Stretch => 8,
        }
}

fn items_of(tag: u8) -> Option<AlignItems> {
    if tag == TAG_UNSET {
        return None;
    }
    let mut value = match tag & !TAG_SAFE {
        0 => AlignItems::START,
        1 => AlignItems::END,
        2 => AlignItems::FLEX_START,
        3 => AlignItems::FLEX_END,
        4 => AlignItems::SELF_START,
        5 => AlignItems::SELF_END,
        6 => AlignItems::CENTER,
        7 => AlignItems::BASELINE,
        8 => AlignItems::STRETCH,
        _ => return None,
    };
    value.safety = safety_of(tag);
    Some(value)
}

fn content_tag(value: Option<AlignContent>) -> u8 {
    let Some(value) = value else {
        return TAG_UNSET;
    };
    safety_tag(value.safety)
        | match value.keyword {
            AlignContentKeyword::Start => 0,
            AlignContentKeyword::End => 1,
            AlignContentKeyword::FlexStart => 2,
            AlignContentKeyword::FlexEnd => 3,
            AlignContentKeyword::Center => 4,
            AlignContentKeyword::Stretch => 5,
            AlignContentKeyword::SpaceBetween => 6,
            AlignContentKeyword::SpaceEvenly => 7,
            AlignContentKeyword::SpaceAround => 8,
        }
}

fn content_of(tag: u8) -> Option<AlignContent> {
    if tag == TAG_UNSET {
        return None;
    }
    let mut value = match tag & !TAG_SAFE {
        0 => AlignContent::START,
        1 => AlignContent::END,
        2 => AlignContent::FLEX_START,
        3 => AlignContent::FLEX_END,
        4 => AlignContent::CENTER,
        5 => AlignContent::STRETCH,
        6 => AlignContent::SPACE_BETWEEN,
        7 => AlignContent::SPACE_EVENLY,
        8 => AlignContent::SPACE_AROUND,
        _ => return None,
    };
    value.safety = safety_of(tag);
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use craie_core::rng::Rng;

    #[test]
    fn row_stays_compact() {
        assert!(std::mem::size_of::<LayoutRow>() <= 136);
    }

    #[test]
    fn default_matches_react_native_style() {
        let want = Style {
            flex_direction: FlexDirection::Column,
            flex_shrink: 0.0,
            ..Style::default()
        };
        assert_style_eq(&LayoutRow::default().to_taffy(), &want);
    }

    /// S6A-05: a present aspect ratio keeps its bits, a NaN payload too.
    #[test]
    fn aspect_ratio_presence_is_separate_from_its_bits() {
        for value in [None, Some(0.0), Some(f32::from_bits(0x7fc0_1234))] {
            let row = LayoutRow::from(&Style {
                aspect_ratio: value,
                ..Style::default()
            });
            assert_eq!(
                row.aspect_ratio().map(f32::to_bits),
                value.map(f32::to_bits)
            );
        }
    }

    #[test]
    fn generated_styles_round_trip() {
        for seed in [1, 2, 3, 0x6a, 0xcafe, 0x5eed] {
            let mut rng = Rng::new(seed);
            for _ in 0..500 {
                let style = gen_style(&mut rng);
                let row = LayoutRow::from(&style);
                let out = row.to_taffy();
                assert_style_eq(&out, &style);
                assert_eq!(LayoutRow::from(&out), row);
            }
        }
    }

    fn gen_style(rng: &mut Rng) -> Style {
        Style {
            display: [Display::Flex, Display::None][rng.below(2) as usize],
            position: [Position::Relative, Position::Absolute][rng.below(2) as usize],
            flex_direction: [
                FlexDirection::Row,
                FlexDirection::Column,
                FlexDirection::RowReverse,
                FlexDirection::ColumnReverse,
            ][rng.below(4) as usize],
            flex_wrap: [FlexWrap::NoWrap, FlexWrap::Wrap, FlexWrap::WrapReverse]
                [rng.below(3) as usize],
            justify_content: gen_content(rng),
            align_items: gen_items(rng),
            align_content: gen_content(rng),
            align_self: gen_items(rng),
            gap: Size {
                width: gen_lp(rng),
                height: gen_lp(rng),
            },
            size: Size {
                width: gen_dim(rng),
                height: gen_dim(rng),
            },
            min_size: Size {
                width: gen_lpa(rng),
                height: gen_lpa(rng),
            },
            max_size: Size {
                width: gen_lpa(rng),
                height: gen_lpa(rng),
            },
            padding: Rect {
                left: gen_lp(rng),
                right: gen_lp(rng),
                top: gen_lp(rng),
                bottom: gen_lp(rng),
            },
            margin: Rect {
                left: gen_lpa(rng),
                right: gen_lpa(rng),
                top: gen_lpa(rng),
                bottom: gen_lpa(rng),
            },
            border: Rect {
                left: gen_lp(rng),
                right: gen_lp(rng),
                top: gen_lp(rng),
                bottom: gen_lp(rng),
            },
            inset: Rect {
                left: gen_lpa(rng),
                right: gen_lpa(rng),
                top: gen_lpa(rng),
                bottom: gen_lpa(rng),
            },
            flex_basis: gen_dim(rng),
            flex_grow: gen_f32(rng),
            flex_shrink: gen_f32(rng),
            aspect_ratio: match rng.below(4) {
                0 => None,
                1 => Some(gen_f32(rng)),
                _ => Some(rng.unit() * 8.0 + 0.1),
            },
            overflow: Point {
                x: gen_overflow(rng),
                y: gen_overflow(rng),
            },
            scrollbar_width: gen_f32(rng),
            direction: [Direction::Ltr, Direction::Rtl][rng.below(2) as usize],
            contain: [
                Contain::NONE,
                Contain::LAYOUT,
                Contain::PAINT,
                Contain::CONTENT,
            ][rng.below(4) as usize],
            box_sizing: [BoxSizing::BorderBox, BoxSizing::ContentBox][rng.below(2) as usize],
            ..Style::default()
        }
    }

    fn gen_f32(rng: &mut Rng) -> f32 {
        match rng.below(8) {
            0 => -0.0,
            1 => 0.0,
            2 => f32::from_bits(0x7fc0_1234),
            _ => rng.unit() * 400.0 - 100.0,
        }
    }

    fn gen_lp(rng: &mut Rng) -> LengthPercentage {
        match rng.below(2) {
            0 => LengthPercentage::length(gen_f32(rng)),
            _ => LengthPercentage::percent(gen_f32(rng)),
        }
    }

    fn gen_lpa(rng: &mut Rng) -> LengthPercentageAuto {
        match rng.below(3) {
            0 => LengthPercentageAuto::length(gen_f32(rng)),
            1 => LengthPercentageAuto::percent(gen_f32(rng)),
            _ => LengthPercentageAuto::auto(),
        }
    }

    fn gen_dim(rng: &mut Rng) -> Dimension {
        match rng.below(10) {
            0 => Dimension::length(gen_f32(rng)),
            1 => Dimension::percent(gen_f32(rng)),
            2 => Dimension::auto(),
            3 => Dimension::min_content(),
            4 => Dimension::max_content(),
            5 => Dimension::fit_content_px(gen_f32(rng)),
            6 => Dimension::fit_content_percent(gen_f32(rng)),
            7 => Dimension::fit_content(),
            8 => Dimension::stretch(),
            _ => Dimension::content(),
        }
    }

    fn gen_overflow(rng: &mut Rng) -> Overflow {
        [
            Overflow::Visible,
            Overflow::Clip,
            Overflow::Hidden,
            Overflow::Scroll,
        ][rng.below(4) as usize]
    }

    fn gen_safety(rng: &mut Rng) -> AlignmentSafety {
        [AlignmentSafety::Unsafe, AlignmentSafety::Safe][rng.below(2) as usize]
    }

    fn gen_items(rng: &mut Rng) -> Option<AlignItems> {
        let value: Option<AlignItems> = [
            None,
            Some(AlignItems::START),
            Some(AlignItems::END),
            Some(AlignItems::FLEX_START),
            Some(AlignItems::FLEX_END),
            Some(AlignItems::SELF_START),
            Some(AlignItems::SELF_END),
            Some(AlignItems::CENTER),
            Some(AlignItems::BASELINE),
            Some(AlignItems::STRETCH),
        ][rng.below(10) as usize];
        value.map(|mut value| {
            value.safety = gen_safety(rng);
            value
        })
    }

    fn gen_content(rng: &mut Rng) -> Option<AlignContent> {
        let value: Option<AlignContent> = [
            None,
            Some(AlignContent::START),
            Some(AlignContent::END),
            Some(AlignContent::FLEX_START),
            Some(AlignContent::FLEX_END),
            Some(AlignContent::CENTER),
            Some(AlignContent::STRETCH),
            Some(AlignContent::SPACE_BETWEEN),
            Some(AlignContent::SPACE_EVENLY),
            Some(AlignContent::SPACE_AROUND),
        ][rng.below(10) as usize];
        value.map(|mut value| {
            value.safety = gen_safety(rng);
            value
        })
    }

    fn assert_style_eq(a: &Style, b: &Style) {
        assert_eq!(a.display, b.display);
        assert_eq!(a.position, b.position);
        assert_eq!(a.flex_direction, b.flex_direction);
        assert_eq!(a.flex_wrap, b.flex_wrap);
        assert_eq!(a.justify_content, b.justify_content);
        assert_eq!(a.align_items, b.align_items);
        assert_eq!(a.align_content, b.align_content);
        assert_eq!(a.align_self, b.align_self);
        assert_lp_size(a.gap, b.gap);
        assert_dim_size(a.size, b.size);
        assert_lpa_size(a.min_size, b.min_size);
        assert_lpa_size(a.max_size, b.max_size);
        assert_lp_rect(a.padding, b.padding);
        assert_lpa_rect(a.margin, b.margin);
        assert_lp_rect(a.border, b.border);
        assert_lpa_rect(a.inset, b.inset);
        assert_dim(a.flex_basis, b.flex_basis);
        assert_bits(a.flex_grow, b.flex_grow);
        assert_bits(a.flex_shrink, b.flex_shrink);
        match (a.aspect_ratio, b.aspect_ratio) {
            (Some(a), Some(b)) => assert_bits(a, b),
            (None, None) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(a.overflow, b.overflow);
        assert_bits(a.scrollbar_width, b.scrollbar_width);
        assert_eq!(a.direction, b.direction);
        assert_eq!(a.contain, b.contain);
        assert_eq!(a.box_sizing, b.box_sizing);
    }

    fn assert_bits(a: f32, b: f32) {
        assert_eq!(a.to_bits(), b.to_bits());
    }

    fn assert_lp(a: LengthPercentage, b: LengthPercentage) {
        match (a.expand(), b.expand()) {
            (ExpandedLengthPercentage::Length(a), ExpandedLengthPercentage::Length(b))
            | (ExpandedLengthPercentage::Percent(a), ExpandedLengthPercentage::Percent(b)) => {
                assert_bits(a, b);
            }
            other => panic!("{other:?}"),
        }
    }

    fn assert_lpa(a: LengthPercentageAuto, b: LengthPercentageAuto) {
        match (a.expand(), b.expand()) {
            (ExpandedLengthPercentageAuto::Length(a), ExpandedLengthPercentageAuto::Length(b))
            | (
                ExpandedLengthPercentageAuto::Percent(a),
                ExpandedLengthPercentageAuto::Percent(b),
            ) => {
                assert_bits(a, b);
            }
            (ExpandedLengthPercentageAuto::Auto, ExpandedLengthPercentageAuto::Auto) => {}
            other => panic!("{other:?}"),
        }
    }

    fn assert_dim(a: Dimension, b: Dimension) {
        match (a.expand(), b.expand()) {
            (ExpandedDimension::Length(a), ExpandedDimension::Length(b))
            | (ExpandedDimension::Percent(a), ExpandedDimension::Percent(b))
            | (ExpandedDimension::FitContentPx(a), ExpandedDimension::FitContentPx(b))
            | (ExpandedDimension::FitContentPercent(a), ExpandedDimension::FitContentPercent(b)) => {
                assert_bits(a, b);
            }
            (ExpandedDimension::Auto, ExpandedDimension::Auto)
            | (ExpandedDimension::MinContent, ExpandedDimension::MinContent)
            | (ExpandedDimension::MaxContent, ExpandedDimension::MaxContent)
            | (ExpandedDimension::FitContent, ExpandedDimension::FitContent)
            | (ExpandedDimension::Stretch, ExpandedDimension::Stretch)
            | (ExpandedDimension::Content, ExpandedDimension::Content) => {}
            other => panic!("{other:?}"),
        }
    }

    fn assert_lp_size(a: Size<LengthPercentage>, b: Size<LengthPercentage>) {
        assert_lp(a.width, b.width);
        assert_lp(a.height, b.height);
    }

    fn assert_lpa_size(a: Size<LengthPercentageAuto>, b: Size<LengthPercentageAuto>) {
        assert_lpa(a.width, b.width);
        assert_lpa(a.height, b.height);
    }

    fn assert_dim_size(a: Size<Dimension>, b: Size<Dimension>) {
        assert_dim(a.width, b.width);
        assert_dim(a.height, b.height);
    }

    fn assert_lp_rect(a: Rect<LengthPercentage>, b: Rect<LengthPercentage>) {
        assert_lp(a.left, b.left);
        assert_lp(a.right, b.right);
        assert_lp(a.top, b.top);
        assert_lp(a.bottom, b.bottom);
    }

    fn assert_lpa_rect(a: Rect<LengthPercentageAuto>, b: Rect<LengthPercentageAuto>) {
        assert_lpa(a.left, b.left);
        assert_lpa(a.right, b.right);
        assert_lpa(a.top, b.top);
        assert_lpa(a.bottom, b.bottom);
    }
}
