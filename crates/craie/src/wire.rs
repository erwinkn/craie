//! Schema-driven binary transaction wire, modeled on gpui-react 46cb47d.
//!
//! One transaction = one React commit. Layout:
//!
//! ```text
//! magic u32 = "CRW1" | version u16 | flags u16 | seq u64 | string_count u32
//! strings: string_count × (u32 byte_len + utf8 bytes)
//! ops:     to end of buffer
//! ```
//!
//! Strings come first so ops can reference them by index. Ops are a flat
//! stream of u8-tagged records with positional fields — no names, no
//! nesting. Decoding borrows string bytes from the buffer; nothing is
//! copied except interned styles.
//!
//! A `style` op carries a u64 presence mask followed by fields in schema
//! order (see `fields` below). Today the encoders write full records; the
//! mask exists so sparse updates don't change the format.

use taffy::{
    AlignContent, AlignItems, Dimension, Display, ExpandedDimension, ExpandedLengthPercentage,
    ExpandedLengthPercentageAuto, FlexDirection, FlexWrap, LengthPercentage, LengthPercentageAuto,
    Overflow, Position, Rect, Size as TSize, Style,
};

use crate::host::{Host, NodeId, NodeKind};
use crate::layout::Layouts;

const MAGIC: u32 = 0x3157_5243; // "CRW1"
const VERSION: u16 = 1;
const NIL: u32 = u32::MAX;

mod op {
    pub const CREATE: u8 = 0x01;
    pub const SET_TEXT: u8 = 0x02;
    pub const TEXT_PROPS: u8 = 0x03;
    pub const SET_STYLE: u8 = 0x04;
    pub const PLACE: u8 = 0x05;
    pub const DETACH: u8 = 0x06;
    pub const REMOVE: u8 = 0x07;
    pub const HIDDEN: u8 = 0x08;
    pub const STYLE: u8 = 0x09;
    pub const VIEW_PAINT: u8 = 0x0A;
}

// Style schema, in mask order. Every field is written as a fixed tag byte
// plus payload where the encoding has a payload.
mod field {
    pub const DISPLAY: u64 = 1 << 0; // u8: 0 flex, 1 none
    pub const POSITION: u64 = 1 << 1; // u8: 0 relative, 1 absolute
    pub const FLEX_DIRECTION: u64 = 1 << 2; // u8: 0 row 1 col 2 row_rev 3 col_rev
    pub const FLEX_WRAP: u64 = 1 << 3; // u8: 0 nowrap 1 wrap 2 wrap_rev
    pub const JUSTIFY_CONTENT: u64 = 1 << 4; // content keyword u8, 0xFF unset
    pub const ALIGN_ITEMS: u64 = 1 << 5; // items keyword u8, 0xFF unset
    pub const ALIGN_CONTENT: u64 = 1 << 6; // content keyword u8, 0xFF unset
    pub const ALIGN_SELF: u64 = 1 << 7; // items keyword u8, 0xFF unset
    pub const GAP: u64 = 1 << 8; // LP ×2 (w, h)
    pub const SIZE: u64 = 1 << 9; // Dimension ×2
    pub const MIN_SIZE: u64 = 1 << 10; // LPA ×2
    pub const MAX_SIZE: u64 = 1 << 11; // LPA ×2
    pub const PADDING: u64 = 1 << 12; // LP ×4 (l, r, t, b)
    pub const MARGIN: u64 = 1 << 13; // LPA ×4
    pub const BORDER: u64 = 1 << 14; // LP ×4
    pub const INSET: u64 = 1 << 15; // LPA ×4
    pub const FLEX_BASIS: u64 = 1 << 16; // Dimension
    pub const FLEX_GROW: u64 = 1 << 17; // f32
    pub const FLEX_SHRINK: u64 = 1 << 18; // f32
    pub const ASPECT_RATIO: u64 = 1 << 19; // f32, NaN = none
    pub const OVERFLOW: u64 = 1 << 20; // u8 ×2 (x, y): 0 visible 1 clip 2 hidden 3 scroll

    pub const ALL: u64 = (1 << 21) - 1;
}

// Alignment keyword tags shared by items/content encodings.
mod kw {
    pub const UNSET: u8 = 0xFF;
    pub const START: u8 = 0;
    pub const END: u8 = 1;
    pub const FLEX_START: u8 = 2;
    pub const FLEX_END: u8 = 3;
    pub const SELF_START: u8 = 4;
    pub const SELF_END: u8 = 5;
    pub const CENTER: u8 = 6;
    pub const BASELINE: u8 = 7;
    pub const STRETCH: u8 = 8;
    pub const SPACE_BETWEEN: u8 = 9;
    pub const SPACE_EVENLY: u8 = 10;
    pub const SPACE_AROUND: u8 = 11;
}

fn items_kw(v: Option<AlignItems>) -> u8 {
    match v.map(|a| a.keyword) {
        None => kw::UNSET,
        Some(taffy::AlignItemsKeyword::Start) => kw::START,
        Some(taffy::AlignItemsKeyword::End) => kw::END,
        Some(taffy::AlignItemsKeyword::FlexStart) => kw::FLEX_START,
        Some(taffy::AlignItemsKeyword::FlexEnd) => kw::FLEX_END,
        Some(taffy::AlignItemsKeyword::SelfStart) => kw::SELF_START,
        Some(taffy::AlignItemsKeyword::SelfEnd) => kw::SELF_END,
        Some(taffy::AlignItemsKeyword::Center) => kw::CENTER,
        Some(taffy::AlignItemsKeyword::Baseline) => kw::BASELINE,
        Some(taffy::AlignItemsKeyword::Stretch) => kw::STRETCH,
    }
}

fn items_of(tag: u8) -> Option<AlignItems> {
    Some(match tag {
        kw::START => AlignItems::START,
        kw::END => AlignItems::END,
        kw::FLEX_START => AlignItems::FLEX_START,
        kw::FLEX_END => AlignItems::FLEX_END,
        kw::SELF_START => AlignItems::SELF_START,
        kw::SELF_END => AlignItems::SELF_END,
        kw::CENTER => AlignItems::CENTER,
        kw::BASELINE => AlignItems::BASELINE,
        kw::STRETCH => AlignItems::STRETCH,
        _ => return None,
    })
}

fn content_kw(v: Option<AlignContent>) -> u8 {
    match v.map(|a| a.keyword) {
        None => kw::UNSET,
        Some(taffy::AlignContentKeyword::Start) => kw::START,
        Some(taffy::AlignContentKeyword::End) => kw::END,
        Some(taffy::AlignContentKeyword::FlexStart) => kw::FLEX_START,
        Some(taffy::AlignContentKeyword::FlexEnd) => kw::FLEX_END,
        Some(taffy::AlignContentKeyword::Center) => kw::CENTER,
        Some(taffy::AlignContentKeyword::Stretch) => kw::STRETCH,
        Some(taffy::AlignContentKeyword::SpaceBetween) => kw::SPACE_BETWEEN,
        Some(taffy::AlignContentKeyword::SpaceEvenly) => kw::SPACE_EVENLY,
        Some(taffy::AlignContentKeyword::SpaceAround) => kw::SPACE_AROUND,
    }
}

fn content_of(tag: u8) -> Option<AlignContent> {
    Some(match tag {
        kw::START => AlignContent::START,
        kw::END => AlignContent::END,
        kw::FLEX_START => AlignContent::FLEX_START,
        kw::FLEX_END => AlignContent::FLEX_END,
        kw::CENTER => AlignContent::CENTER,
        kw::STRETCH => AlignContent::STRETCH,
        kw::SPACE_BETWEEN => AlignContent::SPACE_BETWEEN,
        kw::SPACE_EVENLY => AlignContent::SPACE_EVENLY,
        kw::SPACE_AROUND => AlignContent::SPACE_AROUND,
        _ => return None,
    })
}

// Dimension encodings. LP: 0 length, 1 percent. LPA/Dim add 2 auto and the
// sizing keywords ride the same tag space (3..9).
fn put_lp(out: &mut Vec<u8>, v: LengthPercentage) {
    match v.expand() {
        ExpandedLengthPercentage::Length(f) => {
            out.push(0);
            out.extend_from_slice(&f.to_le_bytes());
        }
        ExpandedLengthPercentage::Percent(f) => {
            out.push(1);
            out.extend_from_slice(&f.to_le_bytes());
        }
    }
}

fn put_lpa(out: &mut Vec<u8>, v: LengthPercentageAuto) {
    match v.expand() {
        ExpandedLengthPercentageAuto::Length(f) => {
            out.push(0);
            out.extend_from_slice(&f.to_le_bytes());
        }
        ExpandedLengthPercentageAuto::Percent(f) => {
            out.push(1);
            out.extend_from_slice(&f.to_le_bytes());
        }
        ExpandedLengthPercentageAuto::Auto => out.push(2),
    }
}

fn put_dim(out: &mut Vec<u8>, v: Dimension) {
    let (tag, val): (u8, Option<f32>) = match v.expand() {
        ExpandedDimension::Length(f) => (0, Some(f)),
        ExpandedDimension::Percent(f) => (1, Some(f)),
        ExpandedDimension::Auto => (2, None),
        ExpandedDimension::MinContent => (3, None),
        ExpandedDimension::MaxContent => (4, None),
        ExpandedDimension::FitContentPx(f) => (5, Some(f)),
        ExpandedDimension::FitContentPercent(f) => (6, Some(f)),
        ExpandedDimension::FitContent => (7, None),
        ExpandedDimension::Stretch => (8, None),
        ExpandedDimension::Content => (9, None),
    };
    out.push(tag);
    if let Some(f) = val {
        out.extend_from_slice(&f.to_le_bytes());
    }
}

fn put_rect_lp(out: &mut Vec<u8>, r: Rect<LengthPercentage>) {
    for v in [r.left, r.right, r.top, r.bottom] {
        put_lp(out, v);
    }
}

fn put_rect_lpa(out: &mut Vec<u8>, r: Rect<LengthPercentageAuto>) {
    for v in [r.left, r.right, r.top, r.bottom] {
        put_lpa(out, v);
    }
}

fn overflow_tag(o: Overflow) -> u8 {
    match o {
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

/// Transaction encoder. Strings are interned per transaction; ops append
/// to a flat op buffer; `finish` splices header + strings + ops.
#[derive(Default)]
pub struct Encoder {
    strings: Vec<String>,
    ops: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Encoder {
        Encoder::default()
    }

    fn str_ref(&mut self, s: &str) -> u32 {
        // Linear scan is fine: transactions carry few unique strings.
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return i as u32;
        }
        self.strings.push(s.to_string());
        (self.strings.len() - 1) as u32
    }

    pub fn create(&mut self, id: u32, kind: u8) {
        self.ops.extend_from_slice(&[op::CREATE]);
        self.ops.extend_from_slice(&id.to_le_bytes());
        self.ops.push(kind);
    }

    pub fn set_text(&mut self, id: u32, text: &str) {
        let s = self.str_ref(text);
        self.ops.push(op::SET_TEXT);
        self.ops.extend_from_slice(&id.to_le_bytes());
        self.ops.extend_from_slice(&s.to_le_bytes());
    }

    pub fn text_props(&mut self, id: u32, font_size: f32, color: u32) {
        self.ops.push(op::TEXT_PROPS);
        self.ops.extend_from_slice(&id.to_le_bytes());
        self.ops.extend_from_slice(&font_size.to_le_bytes());
        self.ops.extend_from_slice(&color.to_le_bytes());
    }

    pub fn set_style(&mut self, id: u32, wire_style: u32) {
        self.ops.push(op::SET_STYLE);
        self.ops.extend_from_slice(&id.to_le_bytes());
        self.ops.extend_from_slice(&wire_style.to_le_bytes());
    }

    pub fn clear_style(&mut self, id: u32) {
        self.set_style(id, NIL);
    }

    /// `before` of `u32::MAX` appends.
    pub fn place(&mut self, parent: u32, child: u32, before: u32) {
        self.ops.push(op::PLACE);
        for v in [parent, child, before] {
            self.ops.extend_from_slice(&v.to_le_bytes());
        }
    }

    pub fn detach(&mut self, id: u32) {
        self.ops.push(op::DETACH);
        self.ops.extend_from_slice(&id.to_le_bytes());
    }

    pub fn remove(&mut self, id: u32) {
        self.ops.push(op::REMOVE);
        self.ops.extend_from_slice(&id.to_le_bytes());
    }

    pub fn hidden(&mut self, id: u32, hidden: bool) {
        self.ops.push(op::HIDDEN);
        self.ops.extend_from_slice(&id.to_le_bytes());
        self.ops.push(hidden as u8);
    }

    /// Sets a view's background color (0xRRGGBBAA).
    pub fn view_paint(&mut self, id: u32, color: u32) {
        self.ops.push(op::VIEW_PAINT);
        self.ops.extend_from_slice(&id.to_le_bytes());
        self.ops.extend_from_slice(&color.to_le_bytes());
    }

    /// Defines wire style `wire_id`. Writes a full record (mask = ALL);
    /// sparse updates are a format-compatible extension.
    pub fn style(&mut self, wire_id: u32, s: &Style) {
        self.ops.push(op::STYLE);
        self.ops.extend_from_slice(&wire_id.to_le_bytes());
        self.ops.extend_from_slice(&field::ALL.to_le_bytes());
        put_style_fields(&mut self.ops, s, field::ALL);
    }

    pub fn finish(&self, seq: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(20 + self.ops.len());
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&seq.to_le_bytes());
        out.extend_from_slice(&(self.strings.len() as u32).to_le_bytes());
        for s in &self.strings {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out.extend_from_slice(&self.ops);
        out
    }
}

/// Serializes the fields selected by `mask` in schema order.
fn put_style_fields(out: &mut Vec<u8>, s: &Style, mask: u64) {
    if mask & field::DISPLAY != 0 {
        out.push(match s.display {
            Display::Flex => 0,
            Display::None => 1,
            #[allow(unreachable_patterns)]
            _ => 0,
        });
    }
    if mask & field::POSITION != 0 {
        out.push(match s.position {
            Position::Relative => 0,
            Position::Absolute => 1,
        });
    }
    if mask & field::FLEX_DIRECTION != 0 {
        out.push(match s.flex_direction {
            FlexDirection::Row => 0,
            FlexDirection::Column => 1,
            FlexDirection::RowReverse => 2,
            FlexDirection::ColumnReverse => 3,
        });
    }
    if mask & field::FLEX_WRAP != 0 {
        out.push(match s.flex_wrap {
            FlexWrap::NoWrap => 0,
            FlexWrap::Wrap => 1,
            FlexWrap::WrapReverse => 2,
            #[allow(unreachable_patterns)]
            _ => 0,
        });
    }
    if mask & field::JUSTIFY_CONTENT != 0 {
        out.push(content_kw(s.justify_content));
    }
    if mask & field::ALIGN_ITEMS != 0 {
        out.push(items_kw(s.align_items));
    }
    if mask & field::ALIGN_CONTENT != 0 {
        out.push(content_kw(s.align_content));
    }
    if mask & field::ALIGN_SELF != 0 {
        out.push(items_kw(s.align_self));
    }
    if mask & field::GAP != 0 {
        put_lp(out, s.gap.width);
        put_lp(out, s.gap.height);
    }
    if mask & field::SIZE != 0 {
        put_dim(out, s.size.width);
        put_dim(out, s.size.height);
    }
    if mask & field::MIN_SIZE != 0 {
        put_lpa(out, s.min_size.width);
        put_lpa(out, s.min_size.height);
    }
    if mask & field::MAX_SIZE != 0 {
        put_lpa(out, s.max_size.width);
        put_lpa(out, s.max_size.height);
    }
    if mask & field::PADDING != 0 {
        put_rect_lp(out, s.padding);
    }
    if mask & field::MARGIN != 0 {
        put_rect_lpa(out, s.margin);
    }
    if mask & field::BORDER != 0 {
        put_rect_lp(out, s.border);
    }
    if mask & field::INSET != 0 {
        put_rect_lpa(out, s.inset);
    }
    if mask & field::FLEX_BASIS != 0 {
        put_dim(out, s.flex_basis);
    }
    if mask & field::FLEX_GROW != 0 {
        out.extend_from_slice(&s.flex_grow.to_le_bytes());
    }
    if mask & field::FLEX_SHRINK != 0 {
        out.extend_from_slice(&s.flex_shrink.to_le_bytes());
    }
    if mask & field::ASPECT_RATIO != 0 {
        out.extend_from_slice(&s.aspect_ratio.unwrap_or(f32::NAN).to_le_bytes());
    }
    if mask & field::OVERFLOW != 0 {
        out.push(overflow_tag(s.overflow.x));
        out.push(overflow_tag(s.overflow.y));
    }
}

// ---------------------------------------------------------------- decoder

#[derive(Debug)]
pub enum WireError {
    Truncated,
    BadMagic,
    BadVersion(u16),
    BadOp(u8),
    BadString(usize),
    BadUtf8,
}

/// A decoded transaction. Strings borrow from the input buffer.
pub struct Txn<'a> {
    pub seq: u64,
    pub ops: Vec<Op<'a>>,
}

#[derive(Debug)]
pub enum Op<'a> {
    Create {
        id: u32,
        kind: u8,
    },
    SetText {
        id: u32,
        text: &'a str,
    },
    TextProps {
        id: u32,
        font_size: f32,
        color: u32,
    },
    SetStyle {
        id: u32,
        wire_style: u32,
    },
    Place {
        parent: u32,
        child: u32,
        before: u32,
    },
    Detach {
        id: u32,
    },
    Remove {
        id: u32,
    },
    Hidden {
        id: u32,
        hidden: bool,
    },
    ViewPaint {
        id: u32,
        color: u32,
    },
    Style {
        wire_id: u32,
        style: Box<Style>,
    },
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.pos + n > self.buf.len() {
            return Err(WireError::Truncated);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, WireError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn lp(&mut self) -> Result<LengthPercentage, WireError> {
        match self.u8()? {
            0 => Ok(LengthPercentage::length(self.f32()?)),
            1 => Ok(LengthPercentage::percent(self.f32()?)),
            _ => Err(WireError::Truncated),
        }
    }

    fn lpa(&mut self) -> Result<LengthPercentageAuto, WireError> {
        match self.u8()? {
            0 => Ok(LengthPercentageAuto::length(self.f32()?)),
            1 => Ok(LengthPercentageAuto::percent(self.f32()?)),
            _ => Ok(LengthPercentageAuto::auto()),
        }
    }

    fn dim(&mut self) -> Result<Dimension, WireError> {
        Ok(match self.u8()? {
            0 => Dimension::length(self.f32()?),
            1 => Dimension::percent(self.f32()?),
            2 => Dimension::auto(),
            3 => Dimension::min_content(),
            4 => Dimension::max_content(),
            5 => Dimension::fit_content_px(self.f32()?),
            6 => Dimension::fit_content_percent(self.f32()?),
            7 => Dimension::fit_content(),
            8 => Dimension::stretch(),
            _ => Dimension::content(),
        })
    }

    fn rect_lp(&mut self) -> Result<Rect<LengthPercentage>, WireError> {
        Ok(Rect {
            left: self.lp()?,
            right: self.lp()?,
            top: self.lp()?,
            bottom: self.lp()?,
        })
    }

    fn rect_lpa(&mut self) -> Result<Rect<LengthPercentageAuto>, WireError> {
        Ok(Rect {
            left: self.lpa()?,
            right: self.lpa()?,
            top: self.lpa()?,
            bottom: self.lpa()?,
        })
    }

    fn style(&mut self, mask: u64) -> Result<Style, WireError> {
        let mut s = Style::default();
        if mask & field::DISPLAY != 0 {
            s.display = match self.u8()? {
                1 => Display::None,
                _ => Display::Flex,
            };
        }
        if mask & field::POSITION != 0 {
            s.position = match self.u8()? {
                1 => Position::Absolute,
                _ => Position::Relative,
            };
        }
        if mask & field::FLEX_DIRECTION != 0 {
            s.flex_direction = match self.u8()? {
                1 => FlexDirection::Column,
                2 => FlexDirection::RowReverse,
                3 => FlexDirection::ColumnReverse,
                _ => FlexDirection::Row,
            };
        }
        if mask & field::FLEX_WRAP != 0 {
            s.flex_wrap = match self.u8()? {
                1 => FlexWrap::Wrap,
                2 => FlexWrap::WrapReverse,
                _ => FlexWrap::NoWrap,
            };
        }
        if mask & field::JUSTIFY_CONTENT != 0 {
            s.justify_content = content_of(self.u8()?);
        }
        if mask & field::ALIGN_ITEMS != 0 {
            s.align_items = items_of(self.u8()?);
        }
        if mask & field::ALIGN_CONTENT != 0 {
            s.align_content = content_of(self.u8()?);
        }
        if mask & field::ALIGN_SELF != 0 {
            s.align_self = items_of(self.u8()?);
        }
        if mask & field::GAP != 0 {
            s.gap = TSize {
                width: self.lp()?,
                height: self.lp()?,
            };
        }
        if mask & field::SIZE != 0 {
            s.size = TSize {
                width: self.dim()?,
                height: self.dim()?,
            };
        }
        if mask & field::MIN_SIZE != 0 {
            s.min_size = TSize {
                width: self.lpa()?,
                height: self.lpa()?,
            };
        }
        if mask & field::MAX_SIZE != 0 {
            s.max_size = TSize {
                width: self.lpa()?,
                height: self.lpa()?,
            };
        }
        if mask & field::PADDING != 0 {
            s.padding = self.rect_lp()?;
        }
        if mask & field::MARGIN != 0 {
            s.margin = self.rect_lpa()?;
        }
        if mask & field::BORDER != 0 {
            s.border = self.rect_lp()?;
        }
        if mask & field::INSET != 0 {
            s.inset = self.rect_lpa()?;
        }
        if mask & field::FLEX_BASIS != 0 {
            s.flex_basis = self.dim()?;
        }
        if mask & field::FLEX_GROW != 0 {
            s.flex_grow = self.f32()?;
        }
        if mask & field::FLEX_SHRINK != 0 {
            s.flex_shrink = self.f32()?;
        }
        if mask & field::ASPECT_RATIO != 0 {
            let v = self.f32()?;
            s.aspect_ratio = if v.is_nan() { None } else { Some(v) };
        }
        if mask & field::OVERFLOW != 0 {
            s.overflow = taffy::Point {
                x: overflow_of(self.u8()?),
                y: overflow_of(self.u8()?),
            };
        }
        Ok(s)
    }
}

/// Decodes a transaction buffer into ops. O(n), zero-copy for strings.
pub fn decode(buf: &[u8]) -> Result<Txn<'_>, WireError> {
    let mut r = Reader { buf, pos: 0 };
    if r.u32()? != MAGIC {
        return Err(WireError::BadMagic);
    }
    let version = r.u16()?;
    if version != VERSION {
        return Err(WireError::BadVersion(version));
    }
    let _flags = r.u16()?;
    let seq = r.u64()?;
    let string_count = r.u32()? as usize;

    let mut strings: Vec<&str> = Vec::with_capacity(string_count);
    for i in 0..string_count {
        let len = r.u32()? as usize;
        let bytes = r.take(len).map_err(|_| WireError::BadString(i))?;
        strings.push(std::str::from_utf8(bytes).map_err(|_| WireError::BadUtf8)?);
    }

    let mut ops = Vec::new();
    while r.pos < buf.len() {
        let tag = r.u8()?;
        let op = match tag {
            op::CREATE => Op::Create {
                id: r.u32()?,
                kind: r.u8()?,
            },
            op::SET_TEXT => Op::SetText {
                id: r.u32()?,
                text: strings
                    .get(r.u32()? as usize)
                    .copied()
                    .ok_or(WireError::Truncated)?,
            },
            op::TEXT_PROPS => Op::TextProps {
                id: r.u32()?,
                font_size: r.f32()?,
                color: r.u32()?,
            },
            op::SET_STYLE => Op::SetStyle {
                id: r.u32()?,
                wire_style: r.u32()?,
            },
            op::PLACE => Op::Place {
                parent: r.u32()?,
                child: r.u32()?,
                before: r.u32()?,
            },
            op::DETACH => Op::Detach { id: r.u32()? },
            op::REMOVE => Op::Remove { id: r.u32()? },
            op::HIDDEN => Op::Hidden {
                id: r.u32()?,
                hidden: r.u8()? != 0,
            },
            op::VIEW_PAINT => Op::ViewPaint {
                id: r.u32()?,
                color: r.u32()?,
            },
            op::STYLE => {
                let wire_id = r.u32()?;
                let mask = r.u64()?;
                Op::Style {
                    wire_id,
                    style: Box::new(r.style(mask)?),
                }
            }
            _ => return Err(WireError::BadOp(tag)),
        };
        ops.push(op);
    }
    Ok(Txn { seq, ops })
}

// ---------------------------------------------------------------- apply

impl Txn<'_> {
    /// Applies the transaction to the retained host and style table.
    /// Ordering is the sender's; the host performs no reordering.
    pub fn apply(&self, host: &mut Host, layouts: &mut Layouts) {
        for op in &self.ops {
            match op {
                Op::Create { id, kind } => {
                    host.create(
                        NodeId(*id),
                        match kind {
                            1 => NodeKind::TEXT,
                            _ => NodeKind::VIEW,
                        },
                    );
                }
                Op::SetText { id, text } => host.set_text(NodeId(*id), text),
                Op::TextProps {
                    id,
                    font_size,
                    color,
                } => host.set_text_props(NodeId(*id), *font_size, *color),
                Op::SetStyle { id, wire_style } => {
                    let sid = if *wire_style == NIL {
                        u32::MAX
                    } else {
                        layouts.style_id_for_wire(*wire_style).0
                    };
                    host.set_style(NodeId(*id), sid);
                }
                Op::Place {
                    parent,
                    child,
                    before,
                } => {
                    host.insert_before(
                        NodeId(*parent),
                        NodeId(*child),
                        if *before == NIL {
                            NodeId::NIL
                        } else {
                            NodeId(*before)
                        },
                    );
                }
                Op::Detach { id } => host.detach(NodeId(*id)),
                Op::Remove { id } => host.remove(NodeId(*id)),
                Op::Hidden { id, hidden } => host.set_hidden(NodeId(*id), *hidden),
                Op::ViewPaint { id, color } => host.set_view_paint(NodeId(*id), *color),
                Op::Style { wire_id, style } => {
                    let existed = layouts.wire_style_defined(*wire_id);
                    layouts.define_style(*wire_id, (**style).clone());
                    if existed {
                        // A redefined style reaches nodes we don't track a
                        // reverse map for; drop every layout cache.
                        layouts.clear_all_caches(host.slot_count());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::{AlignContent, AlignItems};

    fn sample_style() -> Style {
        Style {
            display: Display::Flex,
            position: Position::Absolute,
            flex_direction: FlexDirection::Column,
            flex_wrap: FlexWrap::Wrap,
            justify_content: Some(AlignContent::SPACE_BETWEEN),
            align_items: Some(AlignItems::CENTER),
            align_content: Some(AlignContent::STRETCH),
            align_self: Some(AlignItems::FLEX_END),
            gap: TSize {
                width: LengthPercentage::length(8.0),
                height: LengthPercentage::percent(0.5),
            },
            size: TSize {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            min_size: TSize {
                width: LengthPercentageAuto::length(10.0),
                height: LengthPercentageAuto::auto(),
            },
            padding: Rect {
                left: LengthPercentage::length(1.0),
                right: LengthPercentage::length(2.0),
                top: LengthPercentage::percent(0.25),
                bottom: LengthPercentage::length(4.0),
            },
            margin: Rect {
                left: LengthPercentageAuto::auto(),
                right: LengthPercentageAuto::length(6.0),
                top: LengthPercentageAuto::percent(0.1),
                bottom: LengthPercentageAuto::auto(),
            },
            flex_basis: Dimension::length(64.0),
            flex_grow: 2.0,
            flex_shrink: 0.5,
            aspect_ratio: Some(1.5),
            ..Style::default()
        }
    }

    #[test]
    fn roundtrip_ops() {
        let mut enc = Encoder::new();
        enc.create(0, 0);
        enc.create(1, 1);
        enc.set_text(1, "héllo — مرحبا");
        enc.text_props(1, 16.0, 0xFF00FFEE);
        enc.place(0, 1, u32::MAX);
        enc.hidden(1, true);
        enc.hidden(1, false);
        enc.style(7, &sample_style());
        enc.set_style(0, 7);
        enc.detach(1);
        enc.remove(1);
        let buf = enc.finish(42);

        let txn = decode(&buf).expect("decode");
        assert_eq!(txn.seq, 42);
        assert_eq!(txn.ops.len(), 11);
        match &txn.ops[2] {
            Op::SetText { id, text } => {
                assert_eq!(*id, 1);
                assert_eq!(*text, "héllo — مرحبا");
            }
            _ => panic!("op 2 not SetText"),
        }
        match &txn.ops[7] {
            Op::Style { wire_id, style } => {
                assert_eq!(*wire_id, 7);
                assert_eq!(**style, sample_style());
            }
            _ => panic!("op 7 not Style"),
        }
    }

    #[test]
    fn apply_builds_tree() {
        let mut enc = Encoder::new();
        enc.create(0, 0);
        enc.create(1, 1);
        enc.create(2, 1);
        enc.set_text(1, "first");
        enc.set_text(2, "second");
        enc.place(0, 1, u32::MAX);
        enc.place(0, 2, u32::MAX);
        enc.place(u32::MAX, 0, u32::MAX); // root
        let buf = enc.finish(1);
        let txn = decode(&buf).unwrap();

        let mut host = Host::new();
        let mut layouts = Layouts::new();
        txn.apply(&mut host, &mut layouts);

        assert_eq!(host.len(), 3);
        assert_eq!(
            host.siblings(host.first_child(NodeId(0)))
                .collect::<Vec<_>>(),
            vec![NodeId(1), NodeId(2)]
        );
        assert_eq!(host.text(NodeId(1)).unwrap().text, "first");
        assert!(host.paint_dirty());
    }

    #[test]
    fn apply_style_and_hidden() {
        let mut enc = Encoder::new();
        enc.style(3, &sample_style());
        enc.create(0, 0);
        enc.set_style(0, 3);
        enc.hidden(0, true);
        let buf = enc.finish(1);
        let txn = decode(&buf).unwrap();

        let mut host = Host::new();
        let mut layouts = Layouts::new();
        txn.apply(&mut host, &mut layouts);

        let node = host.node(NodeId(0)).unwrap();
        assert!(node.hidden());
        let style = layouts.style(crate::host::StyleId(node.style));
        assert_eq!(style.flex_direction, FlexDirection::Column);
        assert_eq!(style.padding.left, LengthPercentage::length(1.0));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(decode(&[]), Err(WireError::Truncated)));
        assert!(matches!(
            decode(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
            Err(WireError::BadMagic)
        ));
    }
}
