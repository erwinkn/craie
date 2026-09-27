//! CRW2: the binary transaction wire.
//!
//! One transaction = one React commit. Layout (little endian):
//!
//! ```text
//! magic u32 = "CRW2" | version u16 | flags u16 | seq u64
//! string_count u32 | style_count u32 | span_count u32
//! strings: string_count × (u32 byte_len + utf8 bytes)
//! styles:  style_count × (u64 presence mask + fields in schema order)
//! spans:   span_count × 28 bytes (start u32, font_size f32, color u32,
//!          weight u16, flags u8, reserved u8, family u32 string ref or
//!          NIL, letter_spacing f32, line_height f32)
//! ops:     u8-tagged records to the end of the buffer
//! ```
//!
//! Strings, styles, and spans are per-transaction tables: ops refer to
//! them by index and they die with the transaction. The style table is
//! transport compression only; natively each node owns its layout row.
//! Decoding borrows strings and payload bytes from the buffer and yields
//! the same `Transaction` the Rust direct API builds, so one executor
//! serves both. Op tags group by family (high nibble).

use crate::animation::{Prop, Timing, Transition, Value};
use taffy::{
    AlignContent, AlignItems, Dimension, Display, ExpandedDimension, ExpandedLengthPercentage,
    ExpandedLengthPercentageAuto, FlexDirection, FlexWrap, LengthPercentage, LengthPercentageAuto,
    Overflow, Position, Rect, Size as TSize, Style,
};

use craie_core::geom::Affine;

use crate::image::Fit;
use craie_vector::svg::{self, Drawing, Shape, ShapeKind};
use craie_vector::{FillRule, LineCap, LineJoin, Stroke};

use crate::states::{TermDecl, Values, VariantDecl, layout_key, value_field};

use crate::mutation::{
    Anchor, Claim, Command, ItemDesc, ItemTemplate, Mutation, NIL, NodeKind, Role, SubmitKey,
    TextSpan, Transaction,
};

pub const MAGIC: u32 = 0x3257_5243; // "CRW2"
pub const VERSION: u16 = 5;

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
    /// id u32 | owner u32 (NIL: none): a layer container (`order.rs`).
    pub const LAYER: u8 = 0x22;
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
    /// id u32 (NIL: the window list) | version u32 | count u16 |
    /// count × (kind u8, flags u8, mods u8, 0 u8, key u32)
    pub const CLAIMS: u8 = 0x61;
    // payload
    pub const SURFACE: u8 = 0x70;
    pub const PAYLOAD: u8 = 0x71;
    /// A vector node's runtime drawing (`craie_vector::svg`): id u32 |
    /// view box string u32 | count u16 | count × 45-byte shapes (kind
    /// u8, fill rule u8, join u8, cap u8, current u8, geometry string
    /// u32, transform string u32, dash array string u32, fill u32, stroke
    /// u32, stroke width f32, miter limit f32, dash offset f32, opacity
    /// f32). `current` flags a fill or stroke painting with the node's
    /// inherited color (`svg::CURRENT_FILL`, `CURRENT_STROKE`); its color
    /// is then the tint.
    pub const DRAWING: u8 = 0x72;
    /// An image node's configuration: id u32 | fit u8 (0 cover, 1
    /// contain, 2 fill). Its bytes come as a PAYLOAD.
    pub const IMAGE_CONFIG: u8 = 0x73;
    // command
    pub const COMMAND: u8 = 0x80;
    // lists
    pub const LIST_CONFIG: u8 = 0x90;
    pub const LIST_SPLICE: u8 = 0x91;
    pub const LIST_INDEX: u8 = 0x92;
    pub const SCROLL_ANCHOR: u8 = 0x93;
    // animation
    pub const TRANSITION: u8 = 0xA0;
    pub const ANIMATE: u8 = 0xA1;
    // state styles
    /// id u32 | bits u64 (app bits; input bits reject)
    pub const STATES: u8 = 0xB0;
    /// id u32 | count u16 | count × (term_count u8 | env u8 |
    /// term_count × (scope u32, mask u64) | values); count 0 removes
    pub const VARIANTS: u8 = 0xB1;
    /// narrow_max f32 | compact_max f32
    pub const ENVIRONMENT: u8 = 0xB2;
    /// id u32 | set u8 | color u32
    pub const COLOR: u8 = 0xB3;
}

/// SPATIAL op field mask bits.
pub mod spatial_field {
    /// Six f32: CSS matrix(a, b, c, d, e, f), applied about the center.
    pub const TRANSFORM: u8 = 1 << 0;
    /// One f32 in [0, 1].
    pub const OPACITY: u8 = 1 << 1;
    /// One i32: the order among siblings (higher paints later).
    pub const Z: u8 = 1 << 2;
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
    /// Followed by a string ref: replace the focused input's selection.
    pub const INSERT_TEXT: u8 = 4;
    /// Followed by a string ref: put it on the clipboard.
    pub const WRITE_CLIPBOARD: u8 = 5;
}

/// INPUT_CONFIG flag bits.
pub mod input_flag {
    pub const MULTILINE: u8 = 1 << 0;
    /// Bits 1 and 2: the submit key (`SubmitKey` as u8).
    pub const SUBMIT_SHIFT: u8 = 1;
}

/// Interaction flag bits.
pub mod interaction_flag {
    pub const FOCUSABLE: u8 = 1 << 0;
    pub const SELECTABLE: u8 = 1 << 1;
}

/// Text span flag bits.
pub mod span_flag {
    pub const ITALIC: u8 = 1 << 0;
    pub const UNDERLINE: u8 = 1 << 1;
    pub const LINE_THROUGH: u8 = 1 << 2;
    /// Draw in the nearest inherited color; `color` is the fallback.
    pub const INHERIT_COLOR: u8 = 1 << 3;
    pub const ALL: u8 = ITALIC | UNDERLINE | LINE_THROUGH | INHERIT_COLOR;
}

/// Bytes per span row.
const SPAN_BYTES: usize = 28;

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
                z,
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
                if z.is_some() {
                    mask |= spatial_field::Z;
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
                if let Some(z) = z {
                    u32le(&mut ops, *z as u32);
                }
            }
            Mutation::Layer { id, owner } => {
                ops.push(op::LAYER);
                u32le(&mut ops, *id);
                u32le(&mut ops, *owner);
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
                placeholder,
                multiline,
                submit,
            } => {
                let s = strings.get(placeholder);
                ops.push(op::INPUT_CONFIG);
                u32le(&mut ops, *id);
                f32le(&mut ops, *font_size);
                u32le(&mut ops, s);
                ops.push(*multiline as u8 | (*submit as u8) << input_flag::SUBMIT_SHIFT);
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
                selectable,
            } => {
                ops.push(op::INTERACTION);
                u32le(&mut ops, *id);
                u32le(&mut ops, *listeners);
                ops.push(
                    if *focusable {
                        interaction_flag::FOCUSABLE
                    } else {
                        0
                    } | if *selectable {
                        interaction_flag::SELECTABLE
                    } else {
                        0
                    },
                );
            }
            Mutation::Claims {
                id,
                version,
                claims,
            } => {
                ops.push(op::CLAIMS);
                u32le(&mut ops, *id);
                u32le(&mut ops, *version);
                ops.extend_from_slice(&(claims.len() as u16).to_le_bytes());
                for c in claims.iter() {
                    ops.extend_from_slice(&[c.kind, c.flags, c.mods, 0]);
                    u32le(&mut ops, c.key);
                }
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
            Mutation::Drawing { id, drawing } => {
                let view_box = strings.get(&drawing.view_box);
                ops.push(op::DRAWING);
                u32le(&mut ops, *id);
                u32le(&mut ops, view_box);
                ops.extend_from_slice(&(drawing.shapes.len() as u16).to_le_bytes());
                for sh in &drawing.shapes {
                    let refs = [
                        strings.get(&sh.geometry),
                        strings.get(&sh.transform),
                        strings.get(&sh.dashes),
                    ];
                    ops.extend_from_slice(&[
                        sh.kind as u8,
                        sh.fill_rule as u8,
                        sh.line.join as u8,
                        sh.line.cap as u8,
                        sh.current,
                    ]);
                    for v in refs.into_iter().chain([sh.fill, sh.stroke]) {
                        u32le(&mut ops, v);
                    }
                    for v in [
                        sh.line.width,
                        sh.line.miter_limit,
                        sh.dash_offset,
                        sh.opacity,
                    ] {
                        f32le(&mut ops, v);
                    }
                }
            }
            Mutation::ImageConfig { id, fit } => {
                ops.push(op::IMAGE_CONFIG);
                u32le(&mut ops, *id);
                ops.push(*fit as u8);
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
                    Command::InsertText(t) => {
                        let s = strings.get(t);
                        ops.push(cmd::INSERT_TEXT);
                        u32le(&mut ops, s);
                    }
                    Command::WriteClipboard(t) => {
                        let s = strings.get(t);
                        ops.push(cmd::WRITE_CLIPBOARD);
                        u32le(&mut ops, s);
                    }
                }
            }
            Mutation::ListConfig {
                id,
                overscan,
                fallback,
                templates,
            } => {
                ops.push(op::LIST_CONFIG);
                u32le(&mut ops, *id);
                f32le(&mut ops, *overscan);
                f32le(&mut ops, *fallback);
                ops.extend_from_slice(&(templates.len() as u16).to_le_bytes());
                for t in templates.iter() {
                    f32le(&mut ops, t.base);
                    f32le(&mut ops, t.inset);
                    f32le(&mut ops, t.font_size);
                }
            }
            Mutation::ListSplice {
                id,
                at,
                remove,
                items,
            } => {
                ops.push(op::LIST_SPLICE);
                u32le(&mut ops, *id);
                u32le(&mut ops, *at);
                u32le(&mut ops, *remove);
                u32le(&mut ops, (items.len() / ItemDesc::BYTES) as u32);
                ops.extend_from_slice(items);
            }
            Mutation::ListIndex { id, index } => {
                ops.push(op::LIST_INDEX);
                u32le(&mut ops, *id);
                u32le(&mut ops, *index);
            }
            Mutation::ScrollAnchor { id, anchor } => {
                ops.push(op::SCROLL_ANCHOR);
                u32le(&mut ops, *id);
                ops.push(*anchor as u8);
            }
            Mutation::Transition { id, transitions } => {
                ops.push(op::TRANSITION);
                u32le(&mut ops, *id);
                ops.push(transitions.len() as u8);
                for t in transitions.iter() {
                    ops.push(t.prop as u8);
                    put_timing(&mut ops, &t.timing);
                }
            }
            Mutation::Animate {
                id,
                prop,
                value,
                timing,
            } => {
                ops.push(op::ANIMATE);
                u32le(&mut ops, *id);
                ops.push(*prop as u8);
                put_anim_value(&mut ops, value);
                put_timing(&mut ops, timing);
            }
            Mutation::States { id, bits } => {
                ops.push(op::STATES);
                u32le(&mut ops, *id);
                ops.extend_from_slice(&bits.to_le_bytes());
            }
            Mutation::Variants { id, variants } => {
                ops.push(op::VARIANTS);
                u32le(&mut ops, *id);
                ops.extend_from_slice(&(variants.len() as u16).to_le_bytes());
                for v in variants.iter() {
                    ops.push(v.terms.len() as u8);
                    ops.push(v.env);
                    for t in &v.terms {
                        u32le(&mut ops, t.scope);
                        ops.extend_from_slice(&t.mask.to_le_bytes());
                    }
                    put_values(&mut ops, &v.values);
                }
            }
            Mutation::Environment {
                narrow_max,
                compact_max,
            } => {
                ops.push(op::ENVIRONMENT);
                f32le(&mut ops, *narrow_max);
                f32le(&mut ops, *compact_max);
            }
            Mutation::Color { id, color } => {
                ops.push(op::COLOR);
                u32le(&mut ops, *id);
                ops.push(color.is_some() as u8);
                u32le(&mut ops, color.unwrap_or(0));
            }
        }
    }
    // Span families go through the string table too.
    let family_refs: Vec<u32> = txn.families.iter().map(|f| strings.get(f)).collect();
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
        let mut flags = if sp.italic { span_flag::ITALIC } else { 0 };
        if sp.decoration & 1 != 0 {
            flags |= span_flag::UNDERLINE;
        }
        if sp.decoration & 2 != 0 {
            flags |= span_flag::LINE_THROUGH;
        }
        if sp.inherit_color {
            flags |= span_flag::INHERIT_COLOR;
        }
        out.push(flags);
        out.push(0);
        let family = family_refs.get(sp.family as usize).copied().unwrap_or(NIL);
        out.extend_from_slice(&family.to_le_bytes());
        out.extend_from_slice(&sp.letter_spacing.to_le_bytes());
        out.extend_from_slice(&sp.line_height.to_le_bytes());
    }
    out.extend_from_slice(&ops);
    out
}

/// Writes a style record with only the fields in `mask`, as the JS
/// encoder sends partial styles.
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

#[derive(Debug, PartialEq, Eq)]
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
    txn.spans.reserve(span_count.min(buf.len() / SPAN_BYTES));
    // String ref -> index in `txn.families`.
    let mut family_ix: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for _ in 0..span_count {
        let start = r.u32()?;
        let font_size = r.f32()?;
        let color = r.u32()?;
        let weight = r.u16()?;
        let flags = r.u8()?;
        let _reserved = r.u8()?;
        if flags & !span_flag::ALL != 0 {
            return Err(WireError::BadRef("span flags"));
        }
        let family_ref = r.u32()?;
        let letter_spacing = r.f32()?;
        let line_height = r.f32()?;
        let family = if family_ref == NIL {
            NIL
        } else {
            let name = *strings
                .get(family_ref as usize)
                .ok_or(WireError::BadRef("span family"))?;
            *family_ix.entry(family_ref).or_insert_with(|| {
                txn.families.push(std::borrow::Cow::Borrowed(name));
                txn.families.len() as u32 - 1
            })
        };
        txn.spans.push(TextSpan {
            start,
            font_size,
            color,
            weight,
            italic: flags & span_flag::ITALIC != 0,
            decoration: (flags & span_flag::UNDERLINE != 0) as u8
                | ((flags & span_flag::LINE_THROUGH != 0) as u8) << 1,
            inherit_color: flags & span_flag::INHERIT_COLOR != 0,
            letter_spacing,
            line_height,
            family,
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
                if mask & !(spatial_field::TRANSFORM | spatial_field::OPACITY | spatial_field::Z)
                    != 0
                {
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
                let z = if mask & spatial_field::Z != 0 {
                    Some(r.u32()? as i32)
                } else {
                    None
                };
                Mutation::Spatial {
                    id,
                    transform,
                    opacity,
                    z,
                }
            }
            op::LAYER => Mutation::Layer {
                id: r.u32()?,
                owner: r.u32()?,
            },
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
            op::INPUT_CONFIG => {
                let (id, font_size) = (r.u32()?, r.f32()?);
                let placeholder = string(r.u32()?)?.into();
                let flags = r.u8()?;
                let submit = SubmitKey::from_u8(flags >> input_flag::SUBMIT_SHIFT)
                    .ok_or(WireError::BadRef("input flags"))?;
                Mutation::InputConfig {
                    id,
                    font_size,
                    placeholder,
                    multiline: flags & input_flag::MULTILINE != 0,
                    submit,
                }
            }
            op::ROLE => {
                let id = r.u32()?;
                let role = Role::from_u8(r.u8()?).ok_or(WireError::BadRef("role"))?;
                Mutation::Role { id, role }
            }
            op::LABEL => Mutation::Label {
                id: r.u32()?,
                text: string(r.u32()?)?.into(),
            },
            op::INTERACTION => {
                let (id, listeners, flags) = (r.u32()?, r.u32()?, r.u8()?);
                if flags & !(interaction_flag::FOCUSABLE | interaction_flag::SELECTABLE) != 0 {
                    return Err(WireError::BadRef("interaction flags"));
                }
                Mutation::Interaction {
                    id,
                    listeners,
                    focusable: flags & interaction_flag::FOCUSABLE != 0,
                    selectable: flags & interaction_flag::SELECTABLE != 0,
                }
            }
            op::CLAIMS => {
                let (id, version) = (r.u32()?, r.u32()?);
                let n = r.u16()? as usize;
                let mut claims = Vec::with_capacity(n.min(r.remaining() / 8));
                for _ in 0..n {
                    let [kind, flags, mods, _] = [r.u8()?, r.u8()?, r.u8()?, r.u8()?];
                    let c = Claim {
                        kind,
                        flags,
                        mods,
                        key: r.u32()?,
                    };
                    if !c.valid() {
                        return Err(WireError::BadRef("claim"));
                    }
                    claims.push(c);
                }
                Mutation::Claims {
                    id,
                    version,
                    claims: claims.into(),
                }
            }
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
            op::DRAWING => {
                let id = r.u32()?;
                let view_box = string(r.u32()?)?.into();
                let n = r.u16()? as usize;
                let mut shapes = Vec::with_capacity(n.min(r.remaining() / 45));
                for _ in 0..n {
                    let [kind, rule, join, cap] = [r.u8()?, r.u8()?, r.u8()?, r.u8()?];
                    let current = r.u8()?;
                    let bad = || WireError::BadRef("drawing shape");
                    if current & !(svg::CURRENT_FILL | svg::CURRENT_STROKE) != 0 {
                        return Err(bad());
                    }
                    let kind = ShapeKind::from_u8(kind).ok_or_else(bad)?;
                    let fill_rule = FillRule::from_u8(rule).ok_or_else(bad)?;
                    let join = LineJoin::from_u8(join).ok_or_else(bad)?;
                    let cap = LineCap::from_u8(cap).ok_or_else(bad)?;
                    let geometry = string(r.u32()?)?.into();
                    let transform = string(r.u32()?)?.into();
                    let dashes = string(r.u32()?)?.into();
                    let (fill, stroke) = (r.u32()?, r.u32()?);
                    let (width, miter_limit) = (r.f32()?, r.f32()?);
                    shapes.push(Shape {
                        kind,
                        geometry,
                        transform,
                        fill,
                        fill_rule,
                        stroke,
                        current,
                        line: Stroke {
                            width,
                            join,
                            cap,
                            miter_limit,
                        },
                        dashes,
                        dash_offset: r.f32()?,
                        opacity: r.f32()?,
                    });
                }
                Mutation::Drawing {
                    id,
                    drawing: Drawing { view_box, shapes },
                }
            }
            op::IMAGE_CONFIG => Mutation::ImageConfig {
                id: r.u32()?,
                fit: Fit::from_u8(r.u8()?).ok_or(WireError::BadRef("image fit"))?,
            },
            op::COMMAND => {
                let id = r.u32()?;
                let cmd = match r.u8()? {
                    cmd::FOCUS => Command::Focus,
                    cmd::BLUR => Command::Blur,
                    cmd::SET_TEXT => Command::SetText(string(r.u32()?)?.into()),
                    cmd::SCROLL_TO => Command::ScrollTo(r.f32()?, r.f32()?),
                    cmd::INSERT_TEXT => Command::InsertText(string(r.u32()?)?.into()),
                    cmd::WRITE_CLIPBOARD => Command::WriteClipboard(string(r.u32()?)?.into()),
                    other => return Err(WireError::BadOp(other)),
                };
                Mutation::Command { id, cmd }
            }
            op::LIST_CONFIG => {
                let id = r.u32()?;
                let overscan = r.f32()?;
                let fallback = r.f32()?;
                let n = r.u16()? as usize;
                let mut templates = Vec::with_capacity(n.min(r.remaining() / 12));
                for _ in 0..n {
                    templates.push(ItemTemplate {
                        base: r.f32()?,
                        inset: r.f32()?,
                        font_size: r.f32()?,
                    });
                }
                Mutation::ListConfig {
                    id,
                    overscan,
                    fallback,
                    templates: templates.into(),
                }
            }
            op::LIST_SPLICE => {
                let id = r.u32()?;
                let at = r.u32()?;
                let remove = r.u32()?;
                let count = r.u32()? as usize;
                let len = count
                    .checked_mul(ItemDesc::BYTES)
                    .ok_or(WireError::Truncated)?;
                Mutation::ListSplice {
                    id,
                    at,
                    remove,
                    items: r.take(len)?.into(),
                }
            }
            op::LIST_INDEX => Mutation::ListIndex {
                id: r.u32()?,
                index: r.u32()?,
            },
            op::SCROLL_ANCHOR => {
                let id = r.u32()?;
                let anchor = Anchor::from_u8(r.u8()?).ok_or(WireError::BadRef("anchor"))?;
                Mutation::ScrollAnchor { id, anchor }
            }
            op::TRANSITION => {
                let id = r.u32()?;
                let count = r.u8()? as usize;
                if count > Prop::COUNT {
                    return Err(WireError::BadRef("transition count"));
                }
                let mut transitions = Vec::with_capacity(count);
                for _ in 0..count {
                    let prop =
                        Prop::from_u8(r.u8()?).ok_or(WireError::BadRef("animation property"))?;
                    let timing = r.timing()?;
                    transitions.push(Transition { prop, timing });
                }
                Mutation::Transition {
                    id,
                    transitions: transitions.into(),
                }
            }
            op::ANIMATE => {
                let id = r.u32()?;
                let prop = Prop::from_u8(r.u8()?).ok_or(WireError::BadRef("animation property"))?;
                let value = r.anim_value(prop)?;
                let timing = r.timing()?;
                Mutation::Animate {
                    id,
                    prop,
                    value,
                    timing,
                }
            }
            op::STATES => Mutation::States {
                id: r.u32()?,
                bits: r.u64()?,
            },
            op::VARIANTS => {
                let id = r.u32()?;
                let count = r.u16()? as usize;
                let mut variants = Vec::with_capacity(count.min(r.remaining()));
                for _ in 0..count {
                    let term_count = r.u8()? as usize;
                    let env = r.u8()?;
                    let mut terms = Vec::with_capacity(term_count);
                    for _ in 0..term_count {
                        terms.push(TermDecl {
                            scope: r.u32()?,
                            mask: r.u64()?,
                        });
                    }
                    let values = r.values()?;
                    variants.push(VariantDecl { terms, env, values });
                }
                Mutation::Variants {
                    id,
                    variants: variants.into(),
                }
            }
            op::ENVIRONMENT => Mutation::Environment {
                narrow_max: r.f32()?,
                compact_max: r.f32()?,
            },
            op::COLOR => {
                let id = r.u32()?;
                let set = r.u8()?;
                let color = r.u32()?;
                Mutation::Color {
                    id,
                    color: match set {
                        0 => None,
                        1 => Some(color),
                        _ => return Err(WireError::BadRef("color flag")),
                    },
                }
            }
            _ => return Err(WireError::BadOp(tag)),
        };
        txn.mutations.push(m);
    }
    Ok(txn)
}

fn u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn f32le(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Timing kinds on the wire.
pub mod timing_kind {
    pub const CURVE: u8 = 0;
    pub const SPRING: u8 = 1;
}

/// A timing: kind u8, delay f32, then five f32 (a curve: duration, x1,
/// y1, x2, y2; a spring: stiffness, damping, mass, 0, 0). 25 bytes.
fn put_timing(out: &mut Vec<u8>, t: &Timing) {
    let (kind, v) = match *t {
        Timing::Curve {
            delay,
            duration,
            x1,
            y1,
            x2,
            y2,
        } => (timing_kind::CURVE, [delay, duration, x1, y1, x2, y2]),
        Timing::Spring {
            delay,
            stiffness,
            damping,
            mass,
        } => (
            timing_kind::SPRING,
            [delay, stiffness, damping, mass, 0.0, 0.0],
        ),
    };
    out.push(kind);
    for x in v {
        f32le(out, x);
    }
}

/// An `Animate` target by property: transform 6 f32, opacity f32, a
/// color u32, width or height f32, padding 4 f32 (left, right, top,
/// bottom), gap 2 f32 (column, row). Lengths only.
fn put_anim_value(out: &mut Vec<u8>, v: &Value) {
    let lp = |l: &taffy::LengthPercentage| match l.expand() {
        taffy::style::ExpandedLengthPercentage::Length(v) => v,
        _ => f32::NAN,
    };
    match v {
        Value::Transform(t) => t.0.iter().for_each(|&x| f32le(out, x)),
        Value::Opacity(o) => f32le(out, *o),
        Value::Color(c) => u32le(out, *c),
        Value::Size(d) => f32le(
            out,
            match d.expand() {
                taffy::style::ExpandedDimension::Length(v) => v,
                _ => f32::NAN,
            },
        ),
        Value::Padding(p) => p.iter().for_each(|l| f32le(out, lp(l))),
        Value::Gap(g) => g.iter().for_each(|l| f32le(out, lp(l))),
    }
}

/// Variant values: mask u8, then by bit FILL u32, BORDER_COLOR u32,
/// RADIUS f32, COLOR (set u8, u32), OPACITY f32, TRANSFORM 6 f32, LAYOUT
/// (u64 layout keys, then the style fields they fall in, in schema
/// order), BORDER_WIDTH f32.
fn put_values(out: &mut Vec<u8>, v: &Values) {
    use value_field::*;
    out.push(v.mask);
    if v.mask & FILL != 0 {
        u32le(out, v.fill);
    }
    if v.mask & BORDER_COLOR != 0 {
        u32le(out, v.border.0);
    }
    if v.mask & RADIUS != 0 {
        f32le(out, v.radius);
    }
    if v.mask & COLOR != 0 {
        out.push(v.color.is_some() as u8);
        u32le(out, v.color.unwrap_or(0));
    }
    if v.mask & OPACITY != 0 {
        f32le(out, v.opacity);
    }
    if v.mask & TRANSFORM != 0 {
        v.transform.0.iter().for_each(|&x| f32le(out, x));
    }
    if v.mask & LAYOUT != 0 {
        out.extend_from_slice(&v.layout_keys.to_le_bytes());
        put_style_fields(out, &v.layout.to_taffy(), layout_key::fields(v.layout_keys));
    }
    if v.mask & BORDER_WIDTH != 0 {
        f32le(out, v.border.1);
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn timing(&mut self) -> Result<Timing, WireError> {
        let kind = self.u8()?;
        let mut v = [0.0f32; 6];
        for x in &mut v {
            *x = self.f32()?;
        }
        let [delay, a, b, c, d, e] = v;
        match kind {
            timing_kind::CURVE => Ok(Timing::Curve {
                delay,
                duration: a,
                x1: b,
                y1: c,
                x2: d,
                y2: e,
            }),
            timing_kind::SPRING if d == 0.0 && e == 0.0 => Ok(Timing::Spring {
                delay,
                stiffness: a,
                damping: b,
                mass: c,
            }),
            _ => Err(WireError::BadRef("timing")),
        }
    }

    fn anim_value(&mut self, prop: Prop) -> Result<Value, WireError> {
        let lp = taffy::LengthPercentage::length;
        Ok(match prop {
            Prop::Transform => {
                let mut m = [0.0f32; 6];
                for v in &mut m {
                    *v = self.f32()?;
                }
                Value::Transform(Affine(m))
            }
            Prop::Opacity => Value::Opacity(self.f32()?),
            Prop::Fill | Prop::BorderColor | Prop::Color => Value::Color(self.u32()?),
            Prop::Width | Prop::Height => Value::Size(taffy::Dimension::length(self.f32()?)),
            Prop::Padding => Value::Padding([
                lp(self.f32()?),
                lp(self.f32()?),
                lp(self.f32()?),
                lp(self.f32()?),
            ]),
            Prop::Gap => Value::Gap([lp(self.f32()?), lp(self.f32()?)]),
        })
    }

    fn values(&mut self) -> Result<Values, WireError> {
        use value_field::*;
        let mut v = Values {
            mask: self.u8()?,
            ..Values::default()
        };
        // Never true while `value_field::ALL` fills the `u8`; it stays for
        // when the mask widens.
        #[allow(clippy::bad_bit_mask)]
        if v.mask & !value_field::ALL != 0 {
            return Err(WireError::BadRef("value field"));
        }
        if v.mask & FILL != 0 {
            v.fill = self.u32()?;
        }
        if v.mask & BORDER_COLOR != 0 {
            v.border.0 = self.u32()?;
        }
        if v.mask & RADIUS != 0 {
            v.radius = self.f32()?;
        }
        if v.mask & COLOR != 0 {
            let set = self.u8()?;
            let c = self.u32()?;
            v.color = match set {
                0 => None,
                1 => Some(c),
                _ => return Err(WireError::BadRef("color flag")),
            };
        }
        if v.mask & OPACITY != 0 {
            v.opacity = self.f32()?;
        }
        if v.mask & TRANSFORM != 0 {
            for x in &mut v.transform.0 {
                *x = self.f32()?;
            }
        }
        if v.mask & LAYOUT != 0 {
            v.layout_keys = self.u64()?;
            if v.layout_keys & !layout_key::ALL != 0 {
                return Err(WireError::BadRef("layout key"));
            }
            let fields = layout_key::fields(v.layout_keys);
            v.layout = craie_layout::LayoutRow::from(&self.style(fields)?);
        }
        if v.mask & BORDER_WIDTH != 0 {
            v.border.1 = self.f32()?;
        }
        Ok(v)
    }
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if n > self.remaining() {
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
        let mut s = crate::host::default_style().to_taffy();
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
