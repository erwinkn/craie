//! CRW2: the binary transaction wire.
//!
//! One transaction = one React commit. Layout (little endian):
//!
//! ```text
//! magic u32 = "CRW2" | version u16 | flags u16 | seq u64
//! string_count u32 | style_count u32 | span_count u32
//! strings: string_count × (u32 byte_len + utf8 bytes)
//! styles:  style_count × (u64 presence mask + fields in schema order)
//! spans:   span_count × 16 bytes (start u32, font_size f32, color u32,
//!          weight u16, flags u8, reserved u8)
//! ops:     u8-tagged records to the end of the buffer
//! ```
//!
//! Strings, styles, and spans are per-transaction tables: ops refer to
//! them by index and they die with the transaction. The style table is
//! transport compression only; natively each node owns its layout row.
//! Decoding borrows strings and payload bytes from the buffer and yields
//! the same `Transaction` the Rust direct API builds, so one executor
//! serves both. Op tags group by family (high nibble).

use taffy::{
    AlignContent, AlignItems, Dimension, Display, ExpandedDimension, ExpandedLengthPercentage,
    ExpandedLengthPercentageAuto, FlexDirection, FlexWrap, LengthPercentage, LengthPercentageAuto,
    Overflow, Position, Rect, Size as TSize, Style,
};

use craie_core::geom::Affine;

use crate::mutation::{Command, Mutation, NIL, NodeKind, Role, TextSpan, Transaction};

pub const MAGIC: u32 = 0x3257_5243; // "CRW2"
pub const VERSION: u16 = 2;

pub mod op {
    // structure
    pub const CREATE: u8 = 0x01;
    pub const PLACE: u8 = 0x02;
    pub const DETACH: u8 = 0x03;
    pub const REMOVE: u8 = 0x04;
    // layout
    pub const LAYOUT: u8 = 0x10;
    // spatial
    pub const SPATIAL: u8 = 0x20;
    // paint
    pub const PAINT: u8 = 0x30;
    // text
    pub const PARAGRAPH: u8 = 0x40;
    pub const INPUT_CONFIG: u8 = 0x41;
    // semantics
    pub const ROLE: u8 = 0x50;
    pub const LABEL: u8 = 0x51;
    // interaction
    pub const INTERACTION: u8 = 0x60;
    // payload
    pub const SURFACE: u8 = 0x70;
    pub const PAYLOAD: u8 = 0x71;
    // command
    pub const COMMAND: u8 = 0x80;
}

/// SPATIAL op field mask bits.
pub mod spatial_field {
    /// Six f32: CSS matrix(a, b, c, d, e, f), applied about the center.
    pub const TRANSFORM: u8 = 1 << 0;
    /// One f32 in [0, 1].
    pub const OPACITY: u8 = 1 << 1;
}

/// PAINT op field mask bits.
pub mod paint_field {
    pub const FILL: u8 = 1 << 0;
    pub const RADIUS: u8 = 1 << 1;
    /// border_color u32 + border_width f32, written together.
    pub const BORDER: u8 = 1 << 2;
}

/// COMMAND op sub-tags.
pub mod cmd {
    pub const FOCUS: u8 = 0;
    pub const BLUR: u8 = 1;
    /// Followed by a string ref: replace the input's text.
    pub const SET_TEXT: u8 = 2;
    /// Followed by two f32s: set the scroll offset (logical).
    pub const SCROLL_TO: u8 = 3;
}

/// Text span flag bits.
pub mod span_flag {
    pub const ITALIC: u8 = 1 << 0;
}

// Style schema, in mask order. Every field is written as a fixed tag byte
// plus payload where the encoding has a payload.
pub(crate) mod field {
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

fn overflow_of(tag: u8) -> Result<Overflow, WireError> {
    Ok(match tag {
        0 => Overflow::Visible,
        1 => Overflow::Clip,
        2 => Overflow::Hidden,
        3 => Overflow::Scroll,
        _ => return Err(WireError::BadRef("style field")),
    })
}

/// An alignment keyword: `UNSET` or a tag `parse` knows.
fn keyword<T>(tag: u8, parse: fn(u8) -> Option<T>) -> Result<Option<T>, WireError> {
    if tag == kw::UNSET {
        return Ok(None);
    }
    parse(tag).map(Some).ok_or(WireError::BadRef("style field"))
}

/// Serializes a style's full record (mask = ALL) — the canonical form
/// used both on the wire and as the builder's intern key.
pub fn put_style(out: &mut Vec<u8>, s: &Style) {
    out.extend_from_slice(&field::ALL.to_le_bytes());
    put_style_fields(out, s, field::ALL);
}

/// Per-transaction string table for the encoder.
struct Strings<'t> {
    list: Vec<&'t str>,
    ix: std::collections::HashMap<&'t str, u32>,
}

impl<'t> Strings<'t> {
    fn get(&mut self, s: &'t str) -> u32 {
        if let Some(&i) = self.ix.get(s) {
            return i;
        }
        self.list.push(s);
        let i = self.list.len() as u32 - 1;
        self.ix.insert(s, i);
        i
    }
}

/// Encodes a transaction as CRW2 bytes. Strings are interned per
/// transaction.
pub fn encode(txn: &Transaction<'_>) -> Vec<u8> {
    let mut strings = Strings {
        list: Vec::new(),
        ix: std::collections::HashMap::new(),
    };
    let mut ops: Vec<u8> = Vec::with_capacity(txn.mutations.len() * 12);
    let u32le = |ops: &mut Vec<u8>, v: u32| ops.extend_from_slice(&v.to_le_bytes());
    let f32le = |ops: &mut Vec<u8>, v: f32| ops.extend_from_slice(&v.to_le_bytes());
    for m in &txn.mutations {
        match m {
            Mutation::Create { id, kind } => {
                ops.push(op::CREATE);
                u32le(&mut ops, *id);
                ops.push(*kind as u8);
            }
            Mutation::Place {
                parent,
                child,
                before,
            } => {
                ops.push(op::PLACE);
                for v in [*parent, *child, *before] {
                    u32le(&mut ops, v);
                }
            }
            Mutation::Detach { id } => {
                ops.push(op::DETACH);
                u32le(&mut ops, *id);
            }
            Mutation::Remove { id } => {
                ops.push(op::REMOVE);
                u32le(&mut ops, *id);
            }
            Mutation::Layout { id, style } => {
                ops.push(op::LAYOUT);
                u32le(&mut ops, *id);
                u32le(&mut ops, *style);
            }
            Mutation::Spatial {
                id,
                transform,
                opacity,
            } => {
                ops.push(op::SPATIAL);
                u32le(&mut ops, *id);
                let mut mask = 0;
                if transform.is_some() {
                    mask |= spatial_field::TRANSFORM;
                }
                if opacity.is_some() {
                    mask |= spatial_field::OPACITY;
                }
                ops.push(mask);
                if let Some(t) = transform {
                    for v in t.0 {
                        f32le(&mut ops, v);
                    }
                }
                if let Some(o) = opacity {
                    f32le(&mut ops, *o);
                }
            }
            Mutation::Paint {
                id,
                fill,
                radius,
                border,
            } => {
                ops.push(op::PAINT);
                u32le(&mut ops, *id);
                let mut mask = 0;
                if fill.is_some() {
                    mask |= paint_field::FILL;
                }
                if radius.is_some() {
                    mask |= paint_field::RADIUS;
                }
                if border.is_some() {
                    mask |= paint_field::BORDER;
                }
                ops.push(mask);
                if let Some(c) = fill {
                    u32le(&mut ops, *c);
                }
                if let Some(r) = radius {
                    f32le(&mut ops, *r);
                }
                if let Some((c, w)) = border {
                    u32le(&mut ops, *c);
                    f32le(&mut ops, *w);
                }
            }
            Mutation::Paragraph { id, text, spans } => {
                let s = strings.get(text);
                ops.push(op::PARAGRAPH);
                u32le(&mut ops, *id);
                u32le(&mut ops, s);
                u32le(&mut ops, spans.start);
                u32le(&mut ops, spans.end - spans.start);
            }
            Mutation::InputConfig {
                id,
                font_size,
                color,
                placeholder,
                multiline,
            } => {
                let s = strings.get(placeholder);
                ops.push(op::INPUT_CONFIG);
                u32le(&mut ops, *id);
                f32le(&mut ops, *font_size);
                u32le(&mut ops, *color);
                u32le(&mut ops, s);
                ops.push(*multiline as u8);
            }
            Mutation::Role { id, role } => {
                ops.push(op::ROLE);
                u32le(&mut ops, *id);
                ops.push(*role as u8);
            }
            Mutation::Label { id, text } => {
                let s = strings.get(text);
                ops.push(op::LABEL);
                u32le(&mut ops, *id);
                u32le(&mut ops, s);
            }
            Mutation::Interaction {
                id,
                listeners,
                focusable,
            } => {
                ops.push(op::INTERACTION);
                u32le(&mut ops, *id);
                u32le(&mut ops, *listeners);
                ops.push(*focusable as u8);
            }
            Mutation::Surface { id, kind, params } => {
                ops.push(op::SURFACE);
                u32le(&mut ops, *id);
                u32le(&mut ops, *kind);
                for p in params {
                    u32le(&mut ops, *p);
                }
            }
            Mutation::Payload { id, bytes } => {
                ops.push(op::PAYLOAD);
                u32le(&mut ops, *id);
                u32le(&mut ops, bytes.len() as u32);
                ops.extend_from_slice(bytes);
            }
            Mutation::Command { id, cmd } => {
                ops.push(op::COMMAND);
                u32le(&mut ops, *id);
                match cmd {
                    Command::Focus => ops.push(cmd::FOCUS),
                    Command::Blur => ops.push(cmd::BLUR),
                    Command::SetText(t) => {
                        let s = strings.get(t);
                        ops.push(cmd::SET_TEXT);
                        u32le(&mut ops, s);
                    }
                    Command::ScrollTo(x, y) => {
                        ops.push(cmd::SCROLL_TO);
                        f32le(&mut ops, *x);
                        f32le(&mut ops, *y);
                    }
                }
            }
        }
    }
    let mut out = Vec::with_capacity(28 + ops.len());
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&txn.seq.to_le_bytes());
    out.extend_from_slice(&(strings.list.len() as u32).to_le_bytes());
    out.extend_from_slice(&(txn.styles.len() as u32).to_le_bytes());
    out.extend_from_slice(&(txn.spans.len() as u32).to_le_bytes());
    for s in &strings.list {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    for s in &txn.styles {
        put_style(&mut out, s);
    }
    for sp in &txn.spans {
        out.extend_from_slice(&sp.start.to_le_bytes());
        out.extend_from_slice(&sp.font_size.to_le_bytes());
        out.extend_from_slice(&sp.color.to_le_bytes());
        out.extend_from_slice(&sp.weight.to_le_bytes());
        out.push(if sp.italic { span_flag::ITALIC } else { 0 });
        out.push(0);
    }
    out.extend_from_slice(&ops);
    out
}

/// Writes a style record with only the fields in `mask`, as the JS
/// encoder sends partial styles. Test support for the Rust side.
#[cfg(test)]
pub(crate) fn put_style_masked(out: &mut Vec<u8>, s: &Style, mask: u64) {
    out.extend_from_slice(&mask.to_le_bytes());
    put_style_fields(out, s, mask);
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
    /// A table index, kind, or role out of range.
    BadRef(&'static str),
    /// The transaction decoded but references something invalid: an
    /// absent node, a wrong kind, a placement cycle, a reserved id, or a
    /// malformed paragraph. Nothing was applied.
    Invalid(&'static str),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Decodes a CRW2 buffer. O(n); strings and payloads borrow the buffer.
pub fn decode(buf: &[u8]) -> Result<Transaction<'_>, WireError> {
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
    let style_count = r.u32()? as usize;
    let span_count = r.u32()? as usize;

    let mut strings: Vec<&str> = Vec::with_capacity(string_count.min(buf.len()));
    for i in 0..string_count {
        let len = r.u32()? as usize;
        let bytes = r.take(len).map_err(|_| WireError::BadString(i))?;
        strings.push(std::str::from_utf8(bytes).map_err(|_| WireError::BadUtf8)?);
    }
    let mut txn = Transaction::new(seq);
    txn.styles.reserve(style_count.min(buf.len()));
    for _ in 0..style_count {
        let mask = r.u64()?;
        txn.styles.push(r.style(mask)?);
    }
    txn.spans.reserve(span_count.min(buf.len() / 16));
    for _ in 0..span_count {
        let start = r.u32()?;
        let font_size = r.f32()?;
        let color = r.u32()?;
        let weight = r.u16()?;
        let flags = r.u8()?;
        let _reserved = r.u8()?;
        txn.spans.push(TextSpan {
            start,
            font_size,
            color,
            weight,
            italic: flags & span_flag::ITALIC != 0,
        });
    }
    let string = |i: u32| -> Result<&str, WireError> {
        strings
            .get(i as usize)
            .copied()
            .ok_or(WireError::BadRef("string"))
    };

    while r.pos < buf.len() {
        let tag = r.u8()?;
        let m = match tag {
            op::CREATE => {
                let id = r.u32()?;
                let kind = NodeKind::from_u8(r.u8()?).ok_or(WireError::BadRef("kind"))?;
                Mutation::Create { id, kind }
            }
            op::PLACE => Mutation::Place {
                parent: r.u32()?,
                child: r.u32()?,
                before: r.u32()?,
            },
            op::DETACH => Mutation::Detach { id: r.u32()? },
            op::REMOVE => Mutation::Remove { id: r.u32()? },
            op::LAYOUT => {
                let id = r.u32()?;
                let style = r.u32()?;
                if style != NIL && style as usize >= txn.styles.len() {
                    return Err(WireError::BadRef("style"));
                }
                Mutation::Layout { id, style }
            }
            op::SPATIAL => {
                let id = r.u32()?;
                let mask = r.u8()?;
                if mask & !(spatial_field::TRANSFORM | spatial_field::OPACITY) != 0 {
                    return Err(WireError::BadRef("spatial mask"));
                }
                let transform = if mask & spatial_field::TRANSFORM != 0 {
                    let mut m = [0.0f32; 6];
                    for v in &mut m {
                        *v = r.f32()?;
                    }
                    Some(Affine(m))
                } else {
                    None
                };
                let opacity = if mask & spatial_field::OPACITY != 0 {
                    Some(r.f32()?)
                } else {
                    None
                };
                Mutation::Spatial {
                    id,
                    transform,
                    opacity,
                }
            }
            op::PAINT => {
                let id = r.u32()?;
                let mask = r.u8()?;
                if mask & !(paint_field::FILL | paint_field::RADIUS | paint_field::BORDER) != 0 {
                    return Err(WireError::BadRef("paint mask"));
                }
                let fill = if mask & paint_field::FILL != 0 {
                    Some(r.u32()?)
                } else {
                    None
                };
                let radius = if mask & paint_field::RADIUS != 0 {
                    Some(r.f32()?)
                } else {
                    None
                };
                let border = if mask & paint_field::BORDER != 0 {
                    Some((r.u32()?, r.f32()?))
                } else {
                    None
                };
                Mutation::Paint {
                    id,
                    fill,
                    radius,
                    border,
                }
            }
            op::PARAGRAPH => {
                let id = r.u32()?;
                let text = string(r.u32()?)?;
                let start = r.u32()?;
                let count = r.u32()?;
                let end = start.checked_add(count).ok_or(WireError::BadRef("span"))?;
                if end as usize > txn.spans.len() {
                    return Err(WireError::BadRef("span"));
                }
                Mutation::Paragraph {
                    id,
                    text: text.into(),
                    spans: start..end,
                }
            }
            op::INPUT_CONFIG => Mutation::InputConfig {
                id: r.u32()?,
                font_size: r.f32()?,
                color: r.u32()?,
                placeholder: string(r.u32()?)?.into(),
                multiline: r.u8()? != 0,
            },
            op::ROLE => {
                let id = r.u32()?;
                let role = Role::from_u8(r.u8()?).ok_or(WireError::BadRef("role"))?;
                Mutation::Role { id, role }
            }
            op::LABEL => Mutation::Label {
                id: r.u32()?,
                text: string(r.u32()?)?.into(),
            },
            op::INTERACTION => Mutation::Interaction {
                id: r.u32()?,
                listeners: r.u32()?,
                focusable: r.u8()? != 0,
            },
            op::SURFACE => Mutation::Surface {
                id: r.u32()?,
                kind: r.u32()?,
                params: [r.u32()?, r.u32()?, r.u32()?, r.u32()?],
            },
            op::PAYLOAD => {
                let id = r.u32()?;
                let len = r.u32()? as usize;
                Mutation::Payload {
                    id,
                    bytes: r.take(len)?.into(),
                }
            }
            op::COMMAND => {
                let id = r.u32()?;
                let cmd = match r.u8()? {
                    cmd::FOCUS => Command::Focus,
                    cmd::BLUR => Command::Blur,
                    cmd::SET_TEXT => Command::SetText(string(r.u32()?)?.into()),
                    cmd::SCROLL_TO => Command::ScrollTo(r.f32()?, r.f32()?),
                    other => return Err(WireError::BadOp(other)),
                };
                Mutation::Command { id, cmd }
            }
            _ => return Err(WireError::BadOp(tag)),
        };
        txn.mutations.push(m);
    }
    Ok(txn)
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
            _ => Err(WireError::BadRef("style field")),
        }
    }

    fn lpa(&mut self) -> Result<LengthPercentageAuto, WireError> {
        match self.u8()? {
            0 => Ok(LengthPercentageAuto::length(self.f32()?)),
            1 => Ok(LengthPercentageAuto::percent(self.f32()?)),
            2 => Ok(LengthPercentageAuto::auto()),
            _ => Err(WireError::BadRef("style field")),
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
            9 => Dimension::content(),
            _ => return Err(WireError::BadRef("style field")),
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

    /// Decodes a style over the Craie default (React Native: column).
    /// Unknown mask bits and tags reject the transaction.
    fn style(&mut self, mask: u64) -> Result<Style, WireError> {
        if mask & !field::ALL != 0 {
            return Err(WireError::BadRef("style field"));
        }
        let mut s = crate::host::default_style();
        if mask & field::DISPLAY != 0 {
            s.display = match self.u8()? {
                0 => Display::Flex,
                1 => Display::None,
                _ => return Err(WireError::BadRef("style field")),
            };
        }
        if mask & field::POSITION != 0 {
            s.position = match self.u8()? {
                0 => Position::Relative,
                1 => Position::Absolute,
                _ => return Err(WireError::BadRef("style field")),
            };
        }
        if mask & field::FLEX_DIRECTION != 0 {
            s.flex_direction = match self.u8()? {
                0 => FlexDirection::Row,
                1 => FlexDirection::Column,
                2 => FlexDirection::RowReverse,
                3 => FlexDirection::ColumnReverse,
                _ => return Err(WireError::BadRef("style field")),
            };
        }
        if mask & field::FLEX_WRAP != 0 {
            s.flex_wrap = match self.u8()? {
                0 => FlexWrap::NoWrap,
                1 => FlexWrap::Wrap,
                2 => FlexWrap::WrapReverse,
                _ => return Err(WireError::BadRef("style field")),
            };
        }
        if mask & field::JUSTIFY_CONTENT != 0 {
            s.justify_content = keyword(self.u8()?, content_of)?;
        }
        if mask & field::ALIGN_ITEMS != 0 {
            s.align_items = keyword(self.u8()?, items_of)?;
        }
        if mask & field::ALIGN_CONTENT != 0 {
            s.align_content = keyword(self.u8()?, content_of)?;
        }
        if mask & field::ALIGN_SELF != 0 {
            s.align_self = keyword(self.u8()?, items_of)?;
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
                x: overflow_of(self.u8()?)?,
                y: overflow_of(self.u8()?)?,
            };
        }
        Ok(s)
    }
}
