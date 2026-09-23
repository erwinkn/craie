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
//! (surface kind and bytes), command (focus, blur, set text, scroll).
//!
//! Layout styles and text spans travel in per-transaction tables:
//! mutations refer to them by index, and the tables die with the
//! transaction. Natively every node owns its own copy.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use craie_core::geom::Affine;
use taffy::Style;

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
}

impl NodeKind {
    pub fn from_u8(v: u8) -> Option<NodeKind> {
        Some(match v {
            0 => NodeKind::View,
            1 => NodeKind::Text,
            2 => NodeKind::Input,
            3 => NodeKind::Surface,
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
            _ => return None,
        })
    }
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
}

impl Default for TextSpan {
    fn default() -> TextSpan {
        TextSpan {
            start: 0,
            font_size: 14.0,
            color: 0xFFFF_FFFF,
            weight: 400,
            italic: false,
        }
    }
}

impl TextSpan {
    /// Everything but color: what shaping and line breaking depend on.
    pub fn same_metrics(&self, other: &TextSpan) -> bool {
        self.start == other.start
            && self.font_size == other.font_size
            && self.weight == other.weight
            && self.italic == other.italic
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
    // layout
    /// `style` indexes the transaction's style table; NIL = default.
    Layout {
        id: u32,
        style: u32,
    },
    // spatial
    Spatial {
        id: u32,
        transform: Option<Affine>,
        opacity: Option<f32>,
    },
    // paint
    Paint {
        id: u32,
        fill: Option<u32>,
        radius: Option<f32>,
        border: Option<(u32, f32)>,
    },
    // text
    /// `spans` indexes the transaction's span table.
    Paragraph {
        id: u32,
        text: Cow<'a, str>,
        spans: Range<u32>,
    },
    InputConfig {
        id: u32,
        font_size: f32,
        color: u32,
        placeholder: Cow<'a, str>,
        multiline: bool,
    },
    // semantics
    Role {
        id: u32,
        role: Role,
    },
    /// Empty clears.
    Label {
        id: u32,
        text: Cow<'a, str>,
    },
    // interaction
    Interaction {
        id: u32,
        listeners: u32,
        focusable: bool,
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
    // command
    Command {
        id: u32,
        cmd: Command<'a>,
    },
}

impl Mutation<'_> {
    /// The node the mutation addresses (the child for `Place`).
    pub fn target(&self) -> u32 {
        match *self {
            Mutation::Create { id, .. }
            | Mutation::Detach { id }
            | Mutation::Remove { id }
            | Mutation::Layout { id, .. }
            | Mutation::Spatial { id, .. }
            | Mutation::Paint { id, .. }
            | Mutation::Paragraph { id, .. }
            | Mutation::InputConfig { id, .. }
            | Mutation::Role { id, .. }
            | Mutation::Label { id, .. }
            | Mutation::Interaction { id, .. }
            | Mutation::Surface { id, .. }
            | Mutation::Payload { id, .. }
            | Mutation::Command { id, .. } => id,
            Mutation::Place { child, .. } => child,
        }
    }
}

/// One atomic batch of mutations (one React commit), plus its tables.
#[derive(Clone, Debug, Default)]
pub struct Transaction<'a> {
    pub seq: u64,
    pub styles: Vec<Style>,
    pub spans: Vec<TextSpan>,
    pub mutations: Vec<Mutation<'a>>,
    /// Style interning by encoded bytes (builder only).
    style_ix: HashMap<Vec<u8>, u32>,
    /// Span-list interning by content (builder only).
    span_ix: HashMap<Vec<u32>, Range<u32>>,
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

    pub fn layout(&mut self, id: u32, style: &Style) -> &mut Self {
        let style = self.style(style);
        self.push(Mutation::Layout { id, style })
    }

    pub fn transform(&mut self, id: u32, t: Affine) -> &mut Self {
        self.push(Mutation::Spatial {
            id,
            transform: Some(t),
            opacity: None,
        })
    }

    pub fn opacity(&mut self, id: u32, o: f32) -> &mut Self {
        self.push(Mutation::Spatial {
            id,
            transform: None,
            opacity: Some(o),
        })
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
        })
    }

    pub fn fill(&mut self, id: u32, color: u32) -> &mut Self {
        self.paint(id, Some(color), None, None)
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
                let style = s.weight as u32 | (s.italic as u32) << 16;
                [s.start, s.font_size.to_bits(), s.color, style]
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

    pub fn input_config(
        &mut self,
        id: u32,
        font_size: f32,
        color: u32,
        placeholder: impl Into<Cow<'a, str>>,
        multiline: bool,
    ) -> &mut Self {
        let placeholder = placeholder.into();
        self.push(Mutation::InputConfig {
            id,
            font_size,
            color,
            placeholder,
            multiline,
        })
    }

    pub fn role(&mut self, id: u32, role: Role) -> &mut Self {
        self.push(Mutation::Role { id, role })
    }

    pub fn label(&mut self, id: u32, text: impl Into<Cow<'a, str>>) -> &mut Self {
        self.push(Mutation::Label {
            id,
            text: text.into(),
        })
    }

    pub fn interaction(&mut self, id: u32, listeners: u32, focusable: bool) -> &mut Self {
        self.push(Mutation::Interaction {
            id,
            listeners,
            focusable,
        })
    }

    pub fn surface(&mut self, id: u32, kind: u32, params: [u32; 4]) -> &mut Self {
        self.push(Mutation::Surface { id, kind, params })
    }

    pub fn payload(&mut self, id: u32, bytes: impl Into<Cow<'a, [u8]>>) -> &mut Self {
        self.push(Mutation::Payload {
            id,
            bytes: bytes.into(),
        })
    }

    pub fn command(&mut self, id: u32, cmd: Command<'a>) -> &mut Self {
        self.push(Mutation::Command { id, cmd })
    }
}
