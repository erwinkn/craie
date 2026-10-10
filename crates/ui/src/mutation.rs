//! Semantic mutations: the one vocabulary for changing the host tree.
//!
//! The CRW2 decoder produces a `Transaction` of `Mutation`s; the Rust
//! direct API builds the same `Transaction`; one executor applies both
//! (`Ui::execute`). A transaction is validated whole and applied
//! atomically.
//!
//! Families: structure (create, place, detach, remove), layout (per-node
//! layout inputs), spatial (transform, opacity), paint (fill, border,
//! radius), text (paragraph spans, input configuration), semantics
//! (role, label), interaction (listener mask, focusable), payload
//! (surface kind and bytes, vector drawings), command (focus, blur, set
//! text, scroll), list (templates, item splices, row indices, scroll anchoring).
//!
//! Layout styles and text spans travel in per-transaction tables:
//! mutations refer to them by index, and the tables die with the
//! transaction. Natively every node owns its own copy.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use craie_core::geom::Affine;
use taffy::Style;

pub use crate::claims::Claim;
pub use crate::host::SpatialPatch;
pub use crate::input::SubmitKey;

/// `u32::MAX`: no node / append / root / default style.
pub const NIL: u32 = u32::MAX;

/// Node kind. The wire carries the `u8` value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NodeKind {
    /// A layout/paint container.
    #[default]
    View = 0,
    /// A paragraph leaf.
    Text = 1,
    /// A native text input.
    Input = 2,
    /// A native drawing surface fed by payload bytes.
    Surface = 3,
    /// A virtualized list inside a scroll container: it owns its item
    /// count and extents and lays out only its rendered rows.
    List = 4,
    /// A vector drawing, fitted into its content box: a prepared asset
    /// (`craie_vector::asset`, payload bytes) or runtime shapes
    /// (`Mutation::Drawing`).
    Vector = 5,
    // 6 is reserved.
    /// A raster image: encoded bytes (a payload) the platform decodes
    /// at the size it is shown (`image.rs`), fitted per `ImageConfig`.
    Image = 7,
}

impl NodeKind {
    pub fn from_u8(v: u8) -> Option<NodeKind> {
        Some(match v {
            0 => NodeKind::View,
            1 => NodeKind::Text,
            2 => NodeKind::Input,
            3 => NodeKind::Surface,
            4 => NodeKind::List,
            5 => NodeKind::Vector,
            7 => NodeKind::Image,
            _ => return None,
        })
    }

    /// Kinds that carry a box paint record.
    pub fn has_box(self) -> bool {
        self != NodeKind::Text
    }
}

/// Accessibility role. Explicit on the wire; the facade sets defaults.
/// Native never infers a role from listeners.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Role {
    /// A plain container.
    #[default]
    None = 0,
    Button = 1,
    Label = 2,
    TextInput = 3,
    MultilineTextInput = 4,
    ScrollView = 5,
    Image = 6,
    Heading = 7,
    Link = 8,
    CheckBox = 9,
    Slider = 10,
    List = 11,
    ListItem = 12,
    Group = 13,
    Switch = 14,
    RadioButton = 15,
    RadioGroup = 16,
    Dialog = 17,
    AlertDialog = 18,
    Tab = 19,
    TabList = 20,
}

impl Role {
    pub fn from_u8(v: u8) -> Option<Role> {
        Some(match v {
            0 => Role::None,
            1 => Role::Button,
            2 => Role::Label,
            3 => Role::TextInput,
            4 => Role::MultilineTextInput,
            5 => Role::ScrollView,
            6 => Role::Image,
            7 => Role::Heading,
            8 => Role::Link,
            9 => Role::CheckBox,
            10 => Role::Slider,
            11 => Role::List,
            12 => Role::ListItem,
            13 => Role::Group,
            14 => Role::Switch,
            15 => Role::RadioButton,
            16 => Role::RadioGroup,
            17 => Role::Dialog,
            18 => Role::AlertDialog,
            19 => Role::Tab,
            20 => Role::TabList,
            _ => return None,
        })
    }
}

/// A node's press flags (`Interaction::press`, `press.rs`).
pub mod press {
    /// Presses stop here: press events and `ACTIVATE` go to the
    /// innermost pressable on the path, and to no other.
    pub const PRESSABLE: u8 = 1 << 0;
    /// A disabled pressable: it swallows its presses (nothing fires,
    /// here or further out).
    pub const DISABLED: u8 = 1 << 1;
    /// A press on it or inside it neither moves nor clears focus (the
    /// kit's `preventFocusOnPress`), nor the text selection.
    pub const KEEP_FOCUS: u8 = 1 << 2;
    pub const ALL: u8 = PRESSABLE | DISABLED | KEEP_FOCUS;
}

/// Interaction flag bits.
pub mod interaction_flag {
    pub const FOCUSABLE: u8 = 1 << 0;
    /// Its text descendants form one selection domain.
    pub const SELECTABLE: u8 = 1 << 1;
    /// No hit testing, focus or accessibility for the node and its
    /// subtree (layers it owns excepted).
    pub const INERT: u8 = 1 << 2;
    /// The node a trap focuses when it activates, or when the node
    /// mounts into an active trap the focus is outside of. Nothing
    /// outside traps (LEDGER DF-48).
    pub const AUTO_FOCUS: u8 = 1 << 3;
    /// Bits 4 to 6: the press flags (`press`), shifted.
    pub const PRESS_SHIFT: u8 = 4;
    /// Out of the accessibility tree with its subtree (layers it owns
    /// excepted), input untouched: web `aria-hidden`, React Native's
    /// `accessibilityElementsHidden` (protocol 19).
    pub const A11Y_HIDDEN: u8 = 1 << 7;
    pub const ALL: u8 = FOCUSABLE
        | SELECTABLE
        | INERT
        | AUTO_FOCUS
        | super::press::ALL << PRESS_SHIFT
        | A11Y_HIDDEN;
}

/// Focus trap flag bits (`Mutation::Trap`).
pub mod trap_flag {
    /// Tab cycles inside; clear, the trap is off.
    pub const ACTIVE: u8 = 1 << 0;
    /// Everything outside the trap and the layers it owns is inert.
    pub const MODAL: u8 = 1 << 1;
    /// Activating focuses the trap's `AUTO_FOCUS` node, else its first
    /// focusable, unless focus is already inside.
    pub const AUTO_FOCUS: u8 = 1 << 2;
    /// Deactivating returns focus to where it was on activation.
    pub const RESTORE_FOCUS: u8 = 1 << 3;
    pub const ALL: u8 = ACTIVE | MODAL | AUTO_FOCUS | RESTORE_FOCUS;
}

/// Focus group flag bits (`Mutation::Group`). No bits: not a group.
pub mod group_flag {
    /// ← and → move among the members.
    pub const HORIZONTAL: u8 = 1 << 0;
    /// ↑ and ↓ move among the members.
    pub const VERTICAL: u8 = 1 << 1;
    /// An arrow past an end comes around to the other.
    pub const LOOP: u8 = 1 << 2;
    /// A keyboard move also activates the member it reaches.
    pub const SELECT_ON_FOCUS: u8 = 1 << 3;
    pub const ALL: u8 = HORIZONTAL | VERTICAL | LOOP | SELECT_ON_FOCUS;
}

/// States a node reports to assistive technology even while clear:
/// the facade sets a bit when the state prop was given at all, so
/// `expanded={false}` is "collapsed" and a node without `expanded` is
/// neither. `checked` needs no bit: the check roles always report it,
/// other roles never do. `a11y.rs` reports `SELECTED` on selectable
/// roles only.
pub mod reported {
    pub const EXPANDED: u8 = 1 << 0;
    pub const SELECTED: u8 = 1 << 1;
    pub const ALL: u8 = EXPANDED | SELECTED;
}

/// One style span of a paragraph, starting at byte `start` and running
/// to the next span's start. Span zero starts at 0 and is the base style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSpan {
    pub start: u32,
    /// Logical point size.
    pub font_size: f32,
    /// 0xRRGGBBAA. Lives in the chunk's paint records once drawn.
    pub color: u32,
    /// CSS weight, 100..=900.
    pub weight: u16,
    pub italic: bool,
    /// Decoration flags (`craie_text::decoration`): underline,
    /// line-through. Paint, not metrics.
    pub decoration: u8,
    /// Draw in the nearest inherited color (`COLOR` on the text node or
    /// an ancestor), `color` when there is none.
    pub inherit_color: bool,
    /// A press on this span presses its text node (`press.rs`): a
    /// nested Text with `onPress`. A node-level pressable needs none.
    pub pressable: bool,
    /// This pressable span belongs to the same pressable Text as the
    /// span before it: `<Text onPress>See <Text bold>logs</Text></Text>`
    /// is two spans, one link. A press pressed on one and released on
    /// the other activates.
    pub press_joins: bool,
    /// Added to each cluster's advance, logical points.
    pub letter_spacing: f32,
    /// Absolute line height, logical points; 0: the font's. Span zero's
    /// applies to the whole paragraph.
    pub line_height: f32,
    /// Font family: an index into the transaction's `families` (in the
    /// host, into its family table); `NIL`: the default family.
    pub family: u32,
    /// Tabular digits (OpenType `tnum`).
    pub tabular: bool,
    /// The paragraph's alignment: span zero's applies.
    pub align: craie_text::paragraph::Align,
}

impl Default for TextSpan {
    fn default() -> TextSpan {
        TextSpan {
            start: 0,
            font_size: 14.0,
            color: 0xFFFF_FFFF,
            weight: 400,
            italic: false,
            decoration: 0,
            inherit_color: false,
            pressable: false,
            press_joins: false,
            letter_spacing: 0.0,
            line_height: 0.0,
            family: NIL,
            tabular: false,
            align: craie_text::paragraph::Align::Start,
        }
    }
}

impl TextSpan {
    /// Everything but color: what shaping, line breaking and line
    /// placement depend on.
    pub fn same_metrics(&self, other: &TextSpan) -> bool {
        self.start == other.start
            && self.font_size == other.font_size
            && self.weight == other.weight
            && self.italic == other.italic
            && self.family == other.family
            && self.letter_spacing.to_bits() == other.letter_spacing.to_bits()
            && self.line_height.to_bits() == other.line_height.to_bits()
            && self.tabular == other.tabular
            && self.align == other.align
    }
}

/// A row template: what an item's native estimate is made of besides
/// its text (ARCHITECTURE.md §7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemTemplate {
    /// Fixed extent: padding, headers, gaps (logical points).
    pub base: f32,
    /// Horizontal insets subtracted from the list width before wrapping.
    pub inset: f32,
    /// Font size of the item's wrapping text; 0 = no text.
    pub font_size: f32,
}

/// One item description: its template, its text length in chars, a
/// stable identity (the bridge interns the item's React key; NIL: none),
/// and whether it is the unchanged item that the same splice removes
/// under that identity (a move).
///
/// Identity keeps a scroll anchor and a focused row on their item. Only
/// `unchanged` keeps a measured extent: estimate inputs say nothing about
/// whether the content (and so the height) is the same. The bridge sets
/// it when the new item is the same object as the removed one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemDesc {
    pub template: u16,
    pub text_len: u32,
    pub id: u32,
    pub unchanged: bool,
}

impl ItemDesc {
    /// Wire size of one description (template u16, text length u32,
    /// id u32, flags u8).
    pub const BYTES: usize = 11;
    /// Flags bit: `unchanged`.
    pub const UNCHANGED: u8 = 1;

    /// Decodes packed descriptions (`BYTES` each, little endian).
    pub fn iter(bytes: &[u8]) -> impl Iterator<Item = ItemDesc> + '_ {
        bytes.chunks_exact(Self::BYTES).map(|c| ItemDesc {
            template: u16::from_le_bytes([c[0], c[1]]),
            text_len: u32::from_le_bytes([c[2], c[3], c[4], c[5]]),
            id: u32::from_le_bytes([c[6], c[7], c[8], c[9]]),
            unchanged: c[10] & Self::UNCHANGED != 0,
        })
    }

    /// Packs descriptions for a splice.
    pub fn pack(items: &[ItemDesc]) -> Vec<u8> {
        let mut out = Vec::with_capacity(items.len() * Self::BYTES);
        for d in items {
            out.extend_from_slice(&d.template.to_le_bytes());
            out.extend_from_slice(&d.text_len.to_le_bytes());
            out.extend_from_slice(&d.id.to_le_bytes());
            out.push(if d.unchanged { Self::UNCHANGED } else { 0 });
        }
        out
    }
}

/// A list item's descriptor (protocol 20, `LIST_PATCH`; the lists
/// contract): identity, version, and how to estimate it before its row
/// renders. 16 bytes on the wire: id u32, version u32, template u16,
/// flags u8, a reserved zero byte, then the argument u32: the numeric
/// estimate's f32 bits (`NUMERIC`), else the text length in Unicode
/// scalars for the template.
///
/// A measurement holds while the item keeps its id and version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub id: u32,
    pub version: u32,
    pub template: u16,
    pub flags: u8,
    pub arg: u32,
}

impl Item {
    pub const BYTES: usize = 16;
    /// Its content is loaded (else a placeholder row, content on demand).
    pub const LOADED: u8 = 1 << 0;
    /// `arg` is an f32 estimate, not a text length.
    pub const NUMERIC: u8 = 1 << 1;
    /// Not loaded, and the source gave up: not asked again until the
    /// version changes.
    pub const FAILED: u8 = 1 << 2;
    pub const ALL: u8 = Self::LOADED | Self::NUMERIC | Self::FAILED;

    /// A loaded item estimated at `size` points.
    pub fn sized(id: u32, version: u32, size: f32) -> Item {
        Item {
            id,
            version,
            template: 0,
            flags: Self::LOADED | Self::NUMERIC,
            arg: size.to_bits(),
        }
    }

    pub fn loaded(&self) -> bool {
        self.flags & Self::LOADED != 0
    }

    /// Whether `other` shows the same content: the same version, loaded
    /// and failed state. A measurement holds only while it does (the
    /// kit's rule); a new estimate alone applies while unmeasured.
    pub fn same_content(&self, other: &Item) -> bool {
        let state = Self::LOADED | Self::FAILED;
        self.version == other.version && self.flags & state == other.flags & state
    }

    /// The numeric estimate, if it has one.
    pub fn size(&self) -> Option<f32> {
        (self.flags & Self::NUMERIC != 0).then(|| f32::from_bits(self.arg))
    }

    /// Decodes packed descriptors (`BYTES` each, little endian).
    pub fn iter(bytes: &[u8]) -> impl Iterator<Item = Item> + '_ {
        bytes.chunks_exact(Self::BYTES).map(|c| Item {
            id: u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
            version: u32::from_le_bytes([c[4], c[5], c[6], c[7]]),
            template: u16::from_le_bytes([c[8], c[9]]),
            flags: c[10],
            arg: u32::from_le_bytes([c[12], c[13], c[14], c[15]]),
        })
    }

    pub fn pack(items: &[Item]) -> Vec<u8> {
        let mut out = Vec::with_capacity(items.len() * Self::BYTES);
        for d in items {
            out.extend_from_slice(&d.id.to_le_bytes());
            out.extend_from_slice(&d.version.to_le_bytes());
            out.extend_from_slice(&d.template.to_le_bytes());
            out.push(d.flags);
            out.push(0);
            out.extend_from_slice(&d.arg.to_le_bytes());
        }
        out
    }

    /// Why packed descriptors are malformed, if they are: a partial
    /// descriptor, NIL identity, unknown flag bits, a nonzero reserved
    /// byte, a failed item marked loaded, or a numeric estimate outside
    /// `0..=max`.
    pub fn check(bytes: &[u8], max: f32) -> Result<(), &'static str> {
        if !bytes.len().is_multiple_of(Self::BYTES) {
            return Err("list items not whole descriptors");
        }
        for c in bytes.chunks_exact(Self::BYTES) {
            let d = Item::iter(c).next().unwrap();
            if d.id == NIL {
                return Err("list item without identity");
            }
            if d.flags & !Self::ALL != 0 || c[11] != 0 {
                return Err("unknown list item flags");
            }
            if d.flags & Self::FAILED != 0 && d.loaded() {
                return Err("list item failed and loaded");
            }
            if let Some(s) = d.size()
                && !(s.is_finite() && (0.0..=max).contains(&s))
            {
                return Err("list item estimate out of range");
            }
        }
        Ok(())
    }
}

/// An estimate template of `LIST_CONFIG2` (the lists contract): fixed,
/// by width band, or text at the kit's formula.
#[derive(Clone, Debug, PartialEq)]
pub enum Template {
    Fixed(f32),
    /// (minimum width, size) bands, in any order: the band with the
    /// largest minimum the width reaches, else the one with the smallest
    /// minimum; on equal minimums, the larger size. No band: the list's
    /// fallback.
    Widths(Vec<(f32, f32)>),
    /// `base + line_height × max(1, ceil(text length × font_size ×
    /// char_width / (width − inset)))`.
    Text {
        base: f32,
        inset: f32,
        font_size: f32,
        line_height: f32,
        char_width: f32,
    },
}

impl Template {
    /// Wire kinds: 0 fixed, 1 widths, 2 text.
    pub fn kind(&self) -> u8 {
        match self {
            Template::Fixed(_) => 0,
            Template::Widths(_) => 1,
            Template::Text { .. } => 2,
        }
    }

    /// The estimate of an item with text length `len` at `width`; `None`
    /// for a band template without bands.
    pub fn estimate(&self, len: u32, width: f32) -> Option<f32> {
        match self {
            Template::Fixed(s) => Some(*s),
            Template::Widths(bands) => {
                // By minimum, then size: the last band the width
                // reaches, else the last of those with the first minimum.
                let order = |a: &&(f32, f32), b: &&(f32, f32)| {
                    a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1))
                };
                let reached = bands.iter().filter(|b| width >= b.0).max_by(order);
                let narrowest = || {
                    let min = bands.iter().min_by(order)?.0;
                    bands.iter().filter(|b| b.0 == min).max_by(order)
                };
                reached.or_else(narrowest).map(|b| b.1)
            }
            Template::Text {
                base,
                inset,
                font_size,
                line_height,
                char_width,
            } => {
                // In f64, as the kit computes it in JS; no room is one line.
                let room = width as f64 - *inset as f64;
                let advance = len as f64 * *font_size as f64 * *char_width as f64;
                let lines = if room > 0.0 {
                    (advance / room).ceil().max(1.0)
                } else {
                    1.0
                };
                Some((*base as f64 + *line_height as f64 * lines) as f32)
            }
        }
    }
}

/// One op of a `LIST_PATCH`, against the list as the patch's earlier ops
/// leave it. Descriptors are packed (`Item::BYTES` each).
#[derive(Clone, Debug, PartialEq)]
pub enum ListOp<'a> {
    /// Replaces items `at..at + remove` with `items`.
    Splice {
        at: u32,
        remove: u32,
        items: Cow<'a, [u8]>,
    },
    /// Moves items `from..from + count` to `to`, an index in the list
    /// without them.
    Move { from: u32, count: u32, to: u32 },
    /// New descriptors for items `at..`, with the same identities.
    Update { at: u32, items: Cow<'a, [u8]> },
}

impl<'a> ListOp<'a> {
    /// Wire tags.
    pub const SPLICE: u8 = 0;
    pub const MOVE: u8 = 1;
    pub const UPDATE: u8 = 2;

    pub fn splice(at: u32, remove: u32, items: &[Item]) -> ListOp<'static> {
        ListOp::Splice {
            at,
            remove,
            items: Item::pack(items).into(),
        }
    }

    pub fn update(at: u32, items: &[Item]) -> ListOp<'static> {
        ListOp::Update {
            at,
            items: Item::pack(items).into(),
        }
    }

    /// Packs ops as `LIST_PATCH` carries them: per op a tag, then
    /// splice: at, remove, count u32 and descriptors; move: from, count,
    /// to u32; update: at, count u32 and descriptors.
    pub fn pack(ops: &[ListOp]) -> Vec<u8> {
        let mut out = Vec::new();
        let u32le = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        for op in ops {
            match op {
                ListOp::Splice { at, remove, items } => {
                    out.push(Self::SPLICE);
                    u32le(&mut out, *at);
                    u32le(&mut out, *remove);
                    u32le(&mut out, (items.len() / Item::BYTES) as u32);
                    out.extend_from_slice(items);
                }
                ListOp::Move { from, count, to } => {
                    out.push(Self::MOVE);
                    u32le(&mut out, *from);
                    u32le(&mut out, *count);
                    u32le(&mut out, *to);
                }
                ListOp::Update { at, items } => {
                    out.push(Self::UPDATE);
                    u32le(&mut out, *at);
                    u32le(&mut out, (items.len() / Item::BYTES) as u32);
                    out.extend_from_slice(items);
                }
            }
        }
        out
    }

    /// Reads packed ops, borrowing their descriptors; `None` for a
    /// malformed stream (unknown tag, truncated, trailing bytes).
    pub fn parse(bytes: &'a [u8]) -> Option<Vec<ListOp<'a>>> {
        let mut ops = Vec::new();
        let mut at = 0usize;
        let u32_at = |at: &mut usize| -> Option<u32> {
            let b = bytes.get(*at..*at + 4)?;
            *at += 4;
            Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        while at < bytes.len() {
            let tag = bytes[at];
            at += 1;
            let descs = |at: &mut usize, n: u32| -> Option<&'a [u8]> {
                let len = (n as usize).checked_mul(Item::BYTES)?;
                let b = bytes.get(*at..at.checked_add(len)?)?;
                *at += len;
                Some(b)
            };
            ops.push(match tag {
                Self::SPLICE => {
                    let (a, r, n) = (u32_at(&mut at)?, u32_at(&mut at)?, u32_at(&mut at)?);
                    ListOp::Splice {
                        at: a,
                        remove: r,
                        items: descs(&mut at, n)?.into(),
                    }
                }
                Self::MOVE => ListOp::Move {
                    from: u32_at(&mut at)?,
                    count: u32_at(&mut at)?,
                    to: u32_at(&mut at)?,
                },
                Self::UPDATE => {
                    let (a, n) = (u32_at(&mut at)?, u32_at(&mut at)?);
                    ListOp::Update {
                        at: a,
                        items: descs(&mut at, n)?.into(),
                    }
                }
                _ => return None,
            });
        }
        Some(ops)
    }
}

/// Scroll anchoring policy of a scroll container (§7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Anchor {
    /// The top visible list item keeps its place when extents above it
    /// change.
    #[default]
    KeepVisible = 0,
    /// A scroller at its end stays at its end.
    StickToEnd = 1,
    /// No anchoring.
    None = 2,
}

impl Anchor {
    pub fn from_u8(v: u8) -> Option<Anchor> {
        Some(match v {
            0 => Anchor::KeepVisible,
            1 => Anchor::StickToEnd,
            2 => Anchor::None,
            _ => return None,
        })
    }
}

/// Imperative UI commands.
#[derive(Clone, Debug, PartialEq)]
pub enum Command<'a> {
    Focus,
    Blur,
    /// Replace an input's text.
    SetText(Cow<'a, str>),
    /// Set a scroll container's offset (logical points).
    ScrollTo(f32, f32),
    /// Replace the selection of the focused input, when it is this node
    /// or inside it, as if typed: an undo step and a change event. A
    /// paste claim's answer.
    InsertText(Cow<'a, str>),
    /// Put text on the clipboard: a copy or cut claim's answer.
    WriteClipboard(Cow<'a, str>),
    /// Answer with the node's window-space box from current layout
    /// (`events::out_kind::MEASURE`, keyed by the request).
    Measure(u32),
    /// The window's (node NIL): answer once a frame including this
    /// transaction is presented (`events::out_kind::PRESENTED`), at
    /// rest if asked; with a path, write that frame to a PNG there.
    Present {
        request: u32,
        rest: bool,
        path: Option<Cow<'a, str>>,
    },
}

/// Strings and payload bytes borrow from a decoded buffer or are owned
/// by a direct-API transaction.
#[derive(Clone, Debug, PartialEq)]
pub enum Mutation<'a> {
    // structure
    Create {
        id: u32,
        kind: NodeKind,
    },
    /// `parent` NIL = root level; `before` NIL = append.
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
    /// Ends `id`'s exit if it still runs (`removed`); nothing when it has
    /// ended (its id is free until JS sees the `EXIT_END`).
    EndExit {
        id: u32,
    },
    // layout
    /// `style` indexes the transaction's style table; NIL = default.
    Layout {
        id: u32,
        style: u32,
    },
    // spatial
    /// Transform parts and opacity (`host::Parts`); `z` orders the node
    /// among its siblings (`order.rs`).
    Spatial {
        id: u32,
        patch: SpatialPatch,
        z: Option<i32>,
    },
    /// Makes `id` (a View) a layer container: hit testing passes through
    /// its own box, and it never sorts below the sibling holding `owner`
    /// (NIL: no owner). An owner that is not under `id`'s parent, or is
    /// under `id` itself, holds no sibling: the layer sorts as unowned.
    /// Removing the owner makes it unowned for good.
    Layer {
        id: u32,
        owner: u32,
    },
    // paint
    Paint {
        id: u32,
        fill: Option<u32>,
        radius: Option<f32>,
        border: Option<(u32, f32)>,
        /// Replaces the box shadows (`shadow.rs`); empty: none.
        shadows: Option<crate::shadow::Shadows>,
        /// Replaces the borders per side (`border.rs`); empty: none.
        sides: Option<crate::border::BorderSides>,
    },
    // text
    /// `spans` indexes the transaction's span table.
    Paragraph {
        id: u32,
        text: Cow<'a, str>,
        spans: Range<u32>,
    },
    /// An input's text color is its inherited color (`Color` on the
    /// input or an ancestor), as a span's.
    InputConfig {
        id: u32,
        font_size: f32,
        placeholder: Cow<'a, str>,
        multiline: bool,
        submit: SubmitKey,
    },
    /// A text node's line limit (`craie_text::paragraph::TextStyle::
    /// max_lines`): 0 none; 1 also no wrapping.
    Lines {
        id: u32,
        max: u16,
    },
    // semantics
    /// The role, and the states it reports while clear (`reported`).
    Role {
        id: u32,
        role: Role,
        reported: u8,
    },
    /// Empty clears.
    Label {
        id: u32,
        text: Cow<'a, str>,
    },
    // interaction
    /// `flags`: `interaction_flag` bits.
    Interaction {
        id: u32,
        listeners: u32,
        /// The press flags among them, shifted.
        flags: u8,
    },
    /// Makes the node a focus trap (`trap.rs`), or updates one; `flags`
    /// are `trap_flag` bits, and a trap without `ACTIVE` is inactive.
    Trap {
        id: u32,
        flags: u8,
    },
    /// Makes the node a focus group (`group.rs`), or updates one;
    /// `flags` are `group_flag` bits, and none unmake it.
    Group {
        id: u32,
        flags: u8,
    },
    /// Replaces the node's claims (`claims.rs`; empty clears). `id` NIL
    /// is the window list.
    Claims {
        id: u32,
        version: u32,
        claims: Cow<'a, [Claim]>,
    },
    // payload
    Surface {
        id: u32,
        kind: u32,
        params: [u32; 4],
    },
    Payload {
        id: u32,
        bytes: Cow<'a, [u8]>,
    },
    /// Registers a font file's faces (`Fonts::register`), under `family`
    /// or the file's own names; the window's, no node.
    Font {
        family: Option<Cow<'a, str>>,
        bytes: Cow<'a, [u8]>,
    },
    /// A vector node's drawing from runtime shapes (SVG strings,
    /// `craie_vector::svg`). It replaces an asset payload, and a payload
    /// replaces it.
    Drawing {
        id: u32,
        drawing: craie_vector::svg::Drawing<'a>,
    },
    /// An image node's fit (`image::Fit`).
    ImageConfig {
        id: u32,
        fit: crate::image::Fit,
    },
    // command
    Command {
        id: u32,
        cmd: Command<'a>,
    },
    // lists
    /// Overscan and fallback extent (logical points) and row templates.
    ListConfig {
        id: u32,
        overscan: f32,
        fallback: f32,
        templates: Cow<'a, [ItemTemplate]>,
    },
    /// Replaces items `at..at + remove` with packed descriptions
    /// (`ItemDesc::BYTES` each).
    ListSplice {
        id: u32,
        at: u32,
        remove: u32,
        items: Cow<'a, [u8]>,
    },
    /// Tags row `id` (a child of a list) with its item index; NIL clears.
    ListIndex {
        id: u32,
        index: u32,
    },
    /// The lists contract's configuration (protocol 20): overscan and
    /// lookahead (logical points; a negative lookahead is one viewport
    /// height), retain (viewport heights), the fallback estimate, and the
    /// templates. A new `epoch` drops every measurement.
    ListConfig2 {
        id: u32,
        overscan: f32,
        lookahead: f32,
        retain: f32,
        fallback: f32,
        epoch: u32,
        templates: Cow<'a, [Template]>,
    },
    /// Ops (`ListOp::pack`) taking list `id` from revision `base` to
    /// `next`. A stale base skips the patch with a resync event.
    ListPatch {
        id: u32,
        base: u32,
        next: u32,
        ops: Cow<'a, [u8]>,
    },
    /// Tags row `id` with list `list`'s item `item` at version
    /// `version`: the row is placed at the item wherever it moves, and
    /// measures it only while the item is at that version. Item NIL
    /// clears.
    ListRow {
        id: u32,
        list: u32,
        item: u32,
        version: u32,
    },
    /// Anchoring policy of scroll container `id`.
    ScrollAnchor {
        id: u32,
        anchor: Anchor,
    },
    // animation
    /// Replaces the node's declared transitions (empty clears): later
    /// changes of these properties tween.
    Transition {
        id: u32,
        transitions: Cow<'a, [crate::animation::Transition]>,
    },
    /// Tweens `prop` to `value` once.
    Animate {
        id: u32,
        prop: crate::animation::Prop,
        value: crate::animation::Value,
        timing: crate::animation::Timing,
    },
    /// Keyframe animations (`keyframes.rs`). `Enter` starts them only
    /// in the transaction that creates the node; `Base` replaces the
    /// node's list (equal entries keep running, changed ones restart).
    /// `notify` reports finite ones' ends (`ANIMATION_END`).
    Animation {
        id: u32,
        trigger: crate::keyframes::Trigger,
        notify: bool,
        animations: Cow<'a, [crate::keyframes::Animation]>,
    },
    // state styles
    /// Sets scope `id`'s app state bits (`states::state_bit`); the node
    /// becomes a scope.
    States {
        id: u32,
        bits: u64,
    },
    /// Replaces the node's variant table (empty removes it and restores
    /// the base).
    Variants {
        id: u32,
        variants: Cow<'a, [crate::states::VariantDecl]>,
    },
    /// The window-width breakpoints of `_narrow` and `_compact`.
    Environment {
        narrow_max: f32,
        compact_max: f32,
    },
    /// Sets or clears the color the node's text, inputs and `currentColor`
    /// drawings inherit (its own and its descendants').
    Color {
        id: u32,
        color: Option<u32>,
    },
}

impl Mutation<'_> {
    /// The node the mutation addresses (the child for `Place`).
    pub fn target(&self) -> u32 {
        match *self {
            Mutation::Create { id, .. }
            | Mutation::Detach { id }
            | Mutation::Remove { id }
            | Mutation::EndExit { id }
            | Mutation::Layout { id, .. }
            | Mutation::Spatial { id, .. }
            | Mutation::Layer { id, .. }
            | Mutation::Paint { id, .. }
            | Mutation::Paragraph { id, .. }
            | Mutation::InputConfig { id, .. }
            | Mutation::Lines { id, .. }
            | Mutation::Role { id, .. }
            | Mutation::Label { id, .. }
            | Mutation::Interaction { id, .. }
            | Mutation::Trap { id, .. }
            | Mutation::Group { id, .. }
            | Mutation::Claims { id, .. }
            | Mutation::Surface { id, .. }
            | Mutation::Payload { id, .. }
            | Mutation::Drawing { id, .. }
            | Mutation::ImageConfig { id, .. }
            | Mutation::Command { id, .. }
            | Mutation::ListConfig { id, .. }
            | Mutation::ListSplice { id, .. }
            | Mutation::ListIndex { id, .. }
            | Mutation::ListConfig2 { id, .. }
            | Mutation::ListPatch { id, .. }
            | Mutation::ListRow { id, .. }
            | Mutation::ScrollAnchor { id, .. }
            | Mutation::Transition { id, .. }
            | Mutation::Animate { id, .. }
            | Mutation::Animation { id, .. }
            | Mutation::States { id, .. }
            | Mutation::Variants { id, .. }
            | Mutation::Color { id, .. } => id,
            Mutation::Place { child, .. } => child,
            Mutation::Environment { .. } | Mutation::Font { .. } => NIL,
        }
    }
}

/// One atomic batch of mutations (one React commit), plus its tables.
#[derive(Clone, Debug, Default)]
pub struct Transaction<'a> {
    pub seq: u64,
    pub styles: Vec<Style>,
    pub spans: Vec<TextSpan>,
    /// Font family names spans refer to (`TextSpan::family`).
    pub families: Vec<Cow<'a, str>>,
    pub mutations: Vec<Mutation<'a>>,
    /// Style interning by encoded bytes (builder only).
    style_ix: HashMap<Vec<u8>, u32>,
    /// Span-list interning by content (builder only).
    span_ix: HashMap<Vec<u32>, Range<u32>>,
    /// Family interning by name (builder only).
    family_ix: HashMap<String, u32>,
}

/// Direct-API builder methods. They produce exactly what the decoder
/// produces for the same commit.
impl<'a> Transaction<'a> {
    pub fn new(seq: u64) -> Transaction<'a> {
        Transaction {
            seq,
            ..Transaction::default()
        }
    }

    pub fn push(&mut self, m: Mutation<'a>) -> &mut Self {
        self.mutations.push(m);
        self
    }

    /// Interns a style into the table; identical styles share an index.
    pub fn style(&mut self, style: &Style) -> u32 {
        let mut key = Vec::new();
        crate::wire::put_style(&mut key, style);
        if let Some(&i) = self.style_ix.get(&key) {
            return i;
        }
        let i = self.styles.len() as u32;
        self.styles.push(style.clone());
        self.style_ix.insert(key, i);
        i
    }

    pub fn create(&mut self, id: u32, kind: NodeKind) -> &mut Self {
        self.push(Mutation::Create { id, kind })
    }

    pub fn place(&mut self, parent: u32, child: u32, before: u32) -> &mut Self {
        self.push(Mutation::Place {
            parent,
            child,
            before,
        })
    }

    /// Appends `child` to `parent` (NIL = root level).
    pub fn append(&mut self, parent: u32, child: u32) -> &mut Self {
        self.place(parent, child, NIL)
    }

    pub fn detach(&mut self, id: u32) -> &mut Self {
        self.push(Mutation::Detach { id })
    }

    pub fn remove(&mut self, id: u32) -> &mut Self {
        self.push(Mutation::Remove { id })
    }

    pub fn end_exit(&mut self, id: u32) -> &mut Self {
        self.push(Mutation::EndExit { id })
    }

    pub fn layout(&mut self, id: u32, style: &Style) -> &mut Self {
        let style = self.style(style);
        self.push(Mutation::Layout { id, style })
    }

    /// Writes transform parts and opacity.
    pub fn spatial(&mut self, id: u32, patch: SpatialPatch) -> &mut Self {
        self.push(Mutation::Spatial { id, patch, z: None })
    }

    /// Sets the free matrix (`transform`).
    pub fn transform(&mut self, id: u32, t: Affine) -> &mut Self {
        self.spatial(
            id,
            SpatialPatch {
                matrix: Some(t),
                ..SpatialPatch::default()
            },
        )
    }

    /// Sets the translate: x and y in points, then x and y as fractions
    /// of the node's border box.
    pub fn translate(&mut self, id: u32, t: [f32; 4]) -> &mut Self {
        self.spatial(
            id,
            SpatialPatch {
                translate: Some(t),
                ..SpatialPatch::default()
            },
        )
    }

    /// Sets the rotate, radians clockwise.
    pub fn rotate(&mut self, id: u32, radians: f32) -> &mut Self {
        self.spatial(
            id,
            SpatialPatch {
                rotate: Some(radians),
                ..SpatialPatch::default()
            },
        )
    }

    pub fn scale(&mut self, id: u32, sx: f32, sy: f32) -> &mut Self {
        self.spatial(
            id,
            SpatialPatch {
                scale: Some([sx, sy]),
                ..SpatialPatch::default()
            },
        )
    }

    pub fn opacity(&mut self, id: u32, o: f32) -> &mut Self {
        self.spatial(
            id,
            SpatialPatch {
                opacity: Some(o),
                ..SpatialPatch::default()
            },
        )
    }

    pub fn z(&mut self, id: u32, z: i32) -> &mut Self {
        self.push(Mutation::Spatial {
            id,
            patch: SpatialPatch::default(),
            z: Some(z),
        })
    }

    pub fn layer(&mut self, id: u32, owner: u32) -> &mut Self {
        self.push(Mutation::Layer { id, owner })
    }

    pub fn paint(
        &mut self,
        id: u32,
        fill: Option<u32>,
        radius: Option<f32>,
        border: Option<(u32, f32)>,
    ) -> &mut Self {
        self.push(Mutation::Paint {
            id,
            fill,
            radius,
            border,
            shadows: None,
            sides: None,
        })
    }

    /// Replaces the node's borders per side (top, right, bottom, left).
    /// Sides with `fallback` 0 are all explicit, and paint instead of the
    /// uniform border even at zero width; `BorderSides::default()` (every
    /// side fallen back) removes them.
    pub fn border_sides(&mut self, id: u32, sides: crate::border::BorderSides) -> &mut Self {
        self.push(Mutation::Paint {
            id,
            fill: None,
            radius: None,
            border: None,
            shadows: None,
            sides: Some(sides),
        })
    }

    pub fn fill(&mut self, id: u32, color: u32) -> &mut Self {
        self.paint(id, Some(color), None, None)
    }

    /// Replaces the node's box shadows, first on top. Panics past
    /// `shadow::MAX_SHADOWS`.
    pub fn shadows(&mut self, id: u32, list: &[crate::shadow::Shadow]) -> &mut Self {
        let shadows = crate::shadow::Shadows::new(list).expect("too many shadows");
        self.push(Mutation::Paint {
            id,
            fill: None,
            radius: None,
            border: None,
            shadows: Some(shadows),
            sides: None,
        })
    }

    /// Declares the node's transitions (replacing any).
    pub fn transition(
        &mut self,
        id: u32,
        transitions: &[crate::animation::Transition],
    ) -> &mut Self {
        self.push(Mutation::Transition {
            id,
            transitions: transitions.to_vec().into(),
        })
    }

    /// Tweens one property to `value` (its property is `prop`).
    pub fn animate(
        &mut self,
        id: u32,
        prop: crate::animation::Prop,
        value: crate::animation::Value,
        timing: crate::animation::Timing,
    ) -> &mut Self {
        self.push(Mutation::Animate {
            id,
            prop,
            value,
            timing,
        })
    }

    /// Declares the node's `enter` or own keyframe animations.
    pub fn animation(
        &mut self,
        id: u32,
        trigger: crate::keyframes::Trigger,
        notify: bool,
        animations: &[crate::keyframes::Animation],
    ) -> &mut Self {
        self.push(Mutation::Animation {
            id,
            trigger,
            notify,
            animations: animations.to_vec().into(),
        })
    }

    /// Sets scope `id`'s app state bits.
    pub fn states(&mut self, id: u32, bits: u64) -> &mut Self {
        self.push(Mutation::States { id, bits })
    }

    /// Replaces the node's variant table.
    pub fn variants(&mut self, id: u32, variants: &[crate::states::VariantDecl]) -> &mut Self {
        self.push(Mutation::Variants {
            id,
            variants: variants.to_vec().into(),
        })
    }

    pub fn environment(&mut self, narrow_max: f32, compact_max: f32) -> &mut Self {
        self.push(Mutation::Environment {
            narrow_max,
            compact_max,
        })
    }

    /// Sets or clears the node's inherited color (text, inputs and
    /// `currentColor` drawings).
    pub fn color(&mut self, id: u32, color: Option<u32>) -> &mut Self {
        self.push(Mutation::Color { id, color })
    }

    pub fn paragraph(
        &mut self,
        id: u32,
        text: impl Into<Cow<'a, str>>,
        spans: &[TextSpan],
    ) -> &mut Self {
        // Most paragraphs share one style: intern the span list.
        let key: Vec<u32> = spans
            .iter()
            .flat_map(|s| {
                let style = s.weight as u32
                    | (s.italic as u32) << 16
                    | (s.inherit_color as u32) << 17
                    | (s.pressable as u32) << 18
                    | (s.press_joins as u32) << 19
                    | (s.decoration as u32) << 24;
                [
                    s.start,
                    s.font_size.to_bits(),
                    s.color,
                    style,
                    s.letter_spacing.to_bits(),
                    s.line_height.to_bits(),
                    s.family,
                ]
            })
            .collect();
        let range = match self.span_ix.get(&key) {
            Some(r) => r.clone(),
            None => {
                let start = self.spans.len() as u32;
                self.spans.extend_from_slice(spans);
                let r = start..self.spans.len() as u32;
                self.span_ix.insert(key, r.clone());
                r
            }
        };
        self.push(Mutation::Paragraph {
            id,
            text: text.into(),
            spans: range,
        })
    }

    /// Interns a font family name for `TextSpan::family`.
    pub fn family(&mut self, name: impl Into<Cow<'a, str>>) -> u32 {
        let name = name.into();
        if let Some(&i) = self.family_ix.get(name.as_ref()) {
            return i;
        }
        let i = self.families.len() as u32;
        self.family_ix.insert(name.to_string(), i);
        self.families.push(name);
        i
    }

    /// A single-style paragraph.
    pub fn text(
        &mut self,
        id: u32,
        text: impl Into<Cow<'a, str>>,
        font_size: f32,
        color: u32,
    ) -> &mut Self {
        self.paragraph(
            id,
            text,
            &[TextSpan {
                font_size,
                color,
                ..TextSpan::default()
            }],
        )
    }

    /// The text node's line limit (React Native's `numberOfLines`).
    pub fn lines(&mut self, id: u32, max: u16) -> &mut Self {
        self.push(Mutation::Lines { id, max })
    }

    pub fn input_config(
        &mut self,
        id: u32,
        font_size: f32,
        placeholder: impl Into<Cow<'a, str>>,
        multiline: bool,
    ) -> &mut Self {
        // Enter submits a single line and breaks a multiline one.
        let submit = if multiline {
            SubmitKey::None
        } else {
            SubmitKey::Enter
        };
        self.input_config_submit(id, font_size, placeholder, multiline, submit)
    }

    /// Input config with its submit key.
    pub fn input_config_submit(
        &mut self,
        id: u32,
        font_size: f32,
        placeholder: impl Into<Cow<'a, str>>,
        multiline: bool,
        submit: SubmitKey,
    ) -> &mut Self {
        let placeholder = placeholder.into();
        self.push(Mutation::InputConfig {
            id,
            font_size,
            placeholder,
            multiline,
            submit,
        })
    }

    pub fn role(&mut self, id: u32, role: Role) -> &mut Self {
        self.role_reporting(id, role, 0)
    }

    /// `role`, also reporting the `reported` states while they are clear.
    pub fn role_reporting(&mut self, id: u32, role: Role, reported: u8) -> &mut Self {
        self.push(Mutation::Role { id, role, reported })
    }

    pub fn label(&mut self, id: u32, text: impl Into<Cow<'a, str>>) -> &mut Self {
        self.push(Mutation::Label {
            id,
            text: text.into(),
        })
    }

    pub fn interaction(&mut self, id: u32, listeners: u32, focusable: bool) -> &mut Self {
        self.interaction_flags(id, listeners, focusable, false)
    }

    /// Interaction of a pressable (`press::PRESSABLE` and the other
    /// `press` flags in `press`).
    pub fn interaction_press(
        &mut self,
        id: u32,
        listeners: u32,
        focusable: bool,
        press: u8,
    ) -> &mut Self {
        let focusable = u8::from(focusable) * interaction_flag::FOCUSABLE;
        self.interaction_bits(
            id,
            listeners,
            focusable | press << interaction_flag::PRESS_SHIFT,
        )
    }

    /// Interaction with the focusable and selectable flags: `selectable`
    /// makes the node's text descendants one selection domain.
    pub fn interaction_flags(
        &mut self,
        id: u32,
        listeners: u32,
        focusable: bool,
        selectable: bool,
    ) -> &mut Self {
        let mut flags = 0;
        if focusable {
            flags |= interaction_flag::FOCUSABLE;
        }
        if selectable {
            flags |= interaction_flag::SELECTABLE;
        }
        self.interaction_bits(id, listeners, flags)
    }

    /// Interaction with raw `interaction_flag` bits (`INERT`,
    /// `AUTO_FOCUS`, the shifted press flags, `A11Y_HIDDEN`).
    pub fn interaction_bits(&mut self, id: u32, listeners: u32, flags: u8) -> &mut Self {
        self.push(Mutation::Interaction {
            id,
            listeners,
            flags,
        })
    }

    /// Makes `id` a focus trap with `trap_flag` bits (no `ACTIVE`:
    /// inactive).
    pub fn trap(&mut self, id: u32, flags: u8) -> &mut Self {
        self.push(Mutation::Trap { id, flags })
    }

    /// Makes `id` a focus group with `group_flag` bits (none: not a
    /// group).
    pub fn group(&mut self, id: u32, flags: u8) -> &mut Self {
        self.push(Mutation::Group { id, flags })
    }

    /// The node's claims (NIL: the window list), known to JS as `version`.
    pub fn claims(&mut self, id: u32, version: u32, claims: &[Claim]) -> &mut Self {
        self.push(Mutation::Claims {
            id,
            version,
            claims: claims.to_vec().into(),
        })
    }

    pub fn surface(&mut self, id: u32, kind: u32, params: [u32; 4]) -> &mut Self {
        self.push(Mutation::Surface { id, kind, params })
    }

    /// Registers a font file (TTF, OTF or a collection).
    pub fn font(&mut self, family: Option<&'a str>, bytes: impl Into<Cow<'a, [u8]>>) -> &mut Self {
        self.push(Mutation::Font {
            family: family.map(Cow::Borrowed),
            bytes: bytes.into(),
        })
    }

    pub fn payload(&mut self, id: u32, bytes: impl Into<Cow<'a, [u8]>>) -> &mut Self {
        self.push(Mutation::Payload {
            id,
            bytes: bytes.into(),
        })
    }

    pub fn drawing(&mut self, id: u32, drawing: craie_vector::svg::Drawing<'a>) -> &mut Self {
        self.push(Mutation::Drawing { id, drawing })
    }

    pub fn image_config(&mut self, id: u32, fit: crate::image::Fit) -> &mut Self {
        self.push(Mutation::ImageConfig { id, fit })
    }

    pub fn command(&mut self, id: u32, cmd: Command<'a>) -> &mut Self {
        self.push(Mutation::Command { id, cmd })
    }

    pub fn list_config(
        &mut self,
        id: u32,
        overscan: f32,
        fallback: f32,
        templates: &[ItemTemplate],
    ) -> &mut Self {
        self.push(Mutation::ListConfig {
            id,
            overscan,
            fallback,
            templates: templates.to_vec().into(),
        })
    }

    pub fn list_splice(&mut self, id: u32, at: u32, remove: u32, items: &[ItemDesc]) -> &mut Self {
        self.push(Mutation::ListSplice {
            id,
            at,
            remove,
            items: ItemDesc::pack(items).into(),
        })
    }

    pub fn list_index(&mut self, id: u32, index: u32) -> &mut Self {
        self.push(Mutation::ListIndex { id, index })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn list_config2(
        &mut self,
        id: u32,
        overscan: f32,
        lookahead: f32,
        retain: f32,
        fallback: f32,
        epoch: u32,
        templates: &[Template],
    ) -> &mut Self {
        self.push(Mutation::ListConfig2 {
            id,
            overscan,
            lookahead,
            retain,
            fallback,
            epoch,
            templates: templates.to_vec().into(),
        })
    }

    pub fn list_patch(&mut self, id: u32, base: u32, next: u32, ops: &[ListOp]) -> &mut Self {
        self.push(Mutation::ListPatch {
            id,
            base,
            next,
            ops: ListOp::pack(ops).into(),
        })
    }

    pub fn list_row(&mut self, id: u32, list: u32, item: u32, version: u32) -> &mut Self {
        self.push(Mutation::ListRow {
            id,
            list,
            item,
            version,
        })
    }

    pub fn scroll_anchor(&mut self, id: u32, anchor: Anchor) -> &mut Self {
        self.push(Mutation::ScrollAnchor { id, anchor })
    }
}
