//! Input events, normalized at the platform boundary.
//!
//! The platform adapter produces `Event`s in logical points with winit types
//! resolved to Craie's small enums; `Ui::dispatch` consumes them. Events
//! the JS side subscribes to are encoded by `encode_events` into outbox
//! frames (`crate::bridge`).

/// Keyboard modifier state at event time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Cmd on macOS, Super elsewhere.
    pub meta: bool,
}

impl Mods {
    pub const SHIFT: u8 = 1 << 0;
    pub const CTRL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const META: u8 = 1 << 3;
    /// The chord grammar's `mod`: Cmd on Apple platforms, Ctrl elsewhere.
    pub const COMMAND: u8 = if cfg!(target_os = "macos") {
        Mods::META
    } else {
        Mods::CTRL
    };

    /// The four modifiers as bits (`SHIFT`, `CTRL`, `ALT`, `META`), as
    /// pointer and key records carry them.
    pub fn bits(self) -> u8 {
        self.shift as u8 | (self.ctrl as u8) << 1 | (self.alt as u8) << 2 | (self.meta as u8) << 3
    }

    pub fn from_bits(bits: u8) -> Mods {
        Mods {
            shift: bits & Mods::SHIFT != 0,
            ctrl: bits & Mods::CTRL != 0,
            alt: bits & Mods::ALT != 0,
            meta: bits & Mods::META != 0,
        }
    }
}

/// Named (non-text) keys Craie recognizes. Code values are the wire
/// byte (`code`); the bridge's chord grammar names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Key {
    #[default]
    Unknown,
    Backspace,
    Tab,
    Enter,
    Escape,
    Left,
    Up,
    Right,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Space,
    Insert,
    ContextMenu,
    /// F1 to F24.
    F(u8),
}

impl Key {
    /// Wire code (stable across the bridge).
    pub fn code(self) -> u32 {
        match self {
            Key::Unknown => 0,
            Key::Backspace => 1,
            Key::Tab => 2,
            Key::Enter => 3,
            Key::Escape => 4,
            Key::Left => 5,
            Key::Up => 6,
            Key::Right => 7,
            Key::Down => 8,
            Key::Home => 9,
            Key::End => 10,
            Key::PageUp => 11,
            Key::PageDown => 12,
            Key::Delete => 13,
            Key::Space => 14,
            Key::Insert => 15,
            Key::ContextMenu => 16,
            Key::F(n) => 31 + n.clamp(1, 24) as u32,
        }
    }

    /// The key of a wire code; `None` for codes no key has.
    pub fn from_code(code: u32) -> Option<Key> {
        Some(match code {
            0 => Key::Unknown,
            1 => Key::Backspace,
            2 => Key::Tab,
            3 => Key::Enter,
            4 => Key::Escape,
            5 => Key::Left,
            6 => Key::Up,
            7 => Key::Right,
            8 => Key::Down,
            9 => Key::Home,
            10 => Key::End,
            11 => Key::PageUp,
            12 => Key::PageDown,
            13 => Key::Delete,
            14 => Key::Space,
            15 => Key::Insert,
            16 => Key::ContextMenu,
            32..=55 => Key::F((code - 31) as u8),
            _ => return None,
        })
    }
}

/// A normalized key press/release.
#[derive(Clone, Debug, Default)]
pub struct KeyInput {
    pub key: Key,
    /// Printable text for this press (layout, Shift and Option applied);
    /// `None` for pure named keys and when a command modifier is held.
    pub text: Option<String>,
    /// The character the key gives on the current layout, Shift and
    /// Alt applied but not Ctrl or Cmd, as the web's `event.key` ("O"
    /// for Shift+O, "?" for Shift+/, "с" on a Cyrillic layout), when it
    /// is a character key: what chords match, lower-cased.
    pub char: Option<String>,
    /// Where the key is: the character it gives on a US layout, for
    /// letters, digits and punctuation ('c' for the C position on any
    /// layout). Chords with Alt and a letter or digit, and chords typed
    /// on a non-Latin layout, match it instead of `char`.
    pub code: Option<char>,
    pub mods: Mods,
    /// The platform's auto-repeat of a held key.
    pub repeat: bool,
}

impl KeyInput {
    /// The character a chord compares with (lower-cased): the physical
    /// key's for Alt with a letter or digit (macOS turns Option+I into
    /// "ˆ") and for keys whose layout gives a non-Latin letter (so
    /// `mod+c` copies on a Cyrillic layout), else `char`.
    pub fn chord_char(&self) -> Option<char> {
        let mut chars = self.char.as_deref().unwrap_or("").chars();
        let base = match (chars.next(), chars.next()) {
            (Some(c), None) => c.to_lowercase().next(),
            _ => None,
        };
        match (self.code, base) {
            (Some(p), _) if self.mods.alt && p.is_ascii_alphanumeric() => Some(p),
            (Some(p), Some(b)) if b.is_alphabetic() && !latin(b) => Some(p),
            _ => base,
        }
    }

    /// Whether this press is `mods` plus the character `c`, by the chord
    /// rule: the editing commands' test (`is(Mods::COMMAND, 'c')`).
    pub fn is(&self, mods: u8, c: char) -> bool {
        self.key == Key::Unknown && self.mods.bits() == mods && self.chord_char() == Some(c)
    }
}

/// Latin script: ASCII, Latin-1, Latin Extended-A and -B, and Latin
/// Extended Additional.
fn latin(c: char) -> bool {
    c <= '\u{24F}' || ('\u{1E00}'..='\u{1EFF}').contains(&c)
}

/// Pointer button identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Primary,
    Secondary,
    Middle,
    Other(u16),
}

/// A platform input event, positions in logical points.
#[derive(Clone, Debug)]
pub enum Event {
    PointerMove {
        x: f32,
        y: f32,
    },
    PointerDown {
        x: f32,
        y: f32,
        button: Button,
        mods: Mods,
    },
    PointerUp {
        x: f32,
        y: f32,
        button: Button,
    },
    /// dx/dy in logical points, sign = content scroll direction.
    Wheel {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    KeyDown(KeyInput),
    KeyUp(KeyInput),
    /// IME composing region update; `None` cursor hides the caret.
    ImePreedit {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    ImeCommit(String),
    /// The platform reports composing is done.
    ImeDone,
    /// The pointer left the window: hover ends, and hover at rest stops
    /// testing its last position.
    PointerLeave,
    /// Window focus changed.
    Focus(bool),
    /// Files dropped on the window at (x, y): their paths. The position
    /// is negative when unknown (winit 0.30 reports none).
    Drop {
        x: f32,
        y: f32,
        paths: Vec<String>,
    },
}

// ------------------------------------------------------------- out events

/// Event kinds sent to JS (the `ev` byte of an out event record).
pub mod out_kind {
    pub const POINTER_MOVE: u8 = 1;
    pub const POINTER_DOWN: u8 = 2;
    pub const POINTER_UP: u8 = 3;
    pub const POINTER_ENTER: u8 = 4;
    pub const POINTER_LEAVE: u8 = 5;
    pub const WHEEL: u8 = 6;
    /// A key nothing claimed (`key_bits`); `text` = its `char`.
    pub const KEY_DOWN: u8 = 7;
    pub const KEY_UP: u8 = 8;
    pub const FOCUS: u8 = 9;
    pub const BLUR: u8 = 10;
    /// Text input buffer changed; `text` carries the committed value.
    pub const CHANGE: u8 = 11;
    /// The input's submit key (`input::SubmitKey`) was pressed; `text`
    /// carries the value.
    pub const SUBMIT: u8 = 12;
    /// A scrollable node's offset changed natively; x/y = offset.
    pub const SCROLL: u8 = 13;
    /// A list's rendered range changed (always sent): a = first item,
    /// b = end (exclusive), x = the item kept rendered for focus (-1:
    /// none).
    pub const LIST_RANGE: u8 = 14;
    /// An `Animate` tween ended (always sent): key = property | reason
    /// << 8 (`animation::end_reason`). A keyframe animation's end (when
    /// its op asked): key = index | reason << 8 | (trigger + 1) << 16
    /// (`keyframes::Trigger`); reasons: finished, cancelled (no longer
    /// declared), retargeted (replaced by a changed one), removed (its
    /// node went).
    pub const ANIMATION_END: u8 = 15;
    /// Frame statistics from the platform frame loop, about twice a
    /// second while frames are drawn (node NIL, droppable): x = frames
    /// per second, y = mean CPU time per frame (ms), a = the largest
    /// (ms), b = mean layout and scene time (ms), key = live nodes,
    /// revision = running tweens.
    pub const FRAME_STATS: u8 = 16;
    /// A claimed discrete event (`claims.rs`), always sent: node = the
    /// claiming node (NIL: the window list), key = claim kind | index
    /// << 8, revision = the claim set's version. Paste: `text` = the
    /// clipboard's plain text; copy and cut: the selected text; drop:
    /// the paths, one per line, and x/y; context menu: x/y.
    pub const CLAIM: u8 = 17;
    /// An image node's bytes decoded or failed (`image.rs`), always
    /// sent: key 0 = loaded, x/y = the natural size in pixels; key 1 =
    /// failed, `text` = why.
    pub const IMAGE: u8 = 18;
    /// A primary press on the innermost pressable (`press.rs`): key =
    /// mods | phase << 4 (`press_phase`) | button << 8 | span << 16, as
    /// a pointer record's; x/y window, a/b node-relative.
    pub const PRESS: u8 = 19;
    /// A pressable activated (`press.rs`): key = mods | source << 4
    /// (`activate_source`) | button << 8 | span << 16. x/y are the
    /// release point, or the node's center from a key or assistive
    /// technology; a/b node-relative.
    pub const ACTIVATE: u8 = 20;
    /// The reduced-motion setting changed (node NIL, always sent): key =
    /// the environment bits (`states::env_bit`).
    pub const ENVIRONMENT: u8 = 21;
    /// A detached node's exit ended (`exit.rs`), always sent: node =
    /// the exit's root, key = the reason (`animation::end_reason`:
    /// finished, removed (cut short by a remove), parent gone, skipped).
    /// Native has freed the subtree: its ids may be reused.
    pub const EXIT_END: u8 = 22;
    /// A node with a layout listener has a new border box (`observe.rs`,
    /// always sent): x/y relative to its parent's border box (no scroll
    /// offset, no transform), a/b = width and height. Once after the
    /// listener is set and the node laid out, then on each change.
    pub const LAYOUT: u8 = 23;
    /// The answer to a `Measure` command (always sent): key = the
    /// request, revision = 1 when measured (x/y/a/b = the window-space
    /// bounding box, logical, scroll offsets and transforms applied), 0
    /// when the node is gone, not laid out, or not displayed.
    pub const MEASURE: u8 = 24;
    /// The window's state changed (node NIL, always sent): x/y = the
    /// logical size, a = the scale factor, key = `window_bit`s.
    pub const WINDOW: u8 = 25;
    /// The answer to a `Present` command (node NIL, always sent): key =
    /// the request, revision = the presented frame's number, x/y = its
    /// size in pixels, `text` = why its capture failed ("": none).
    pub const PRESENTED: u8 = 26;
}

/// Whether events of `kind` must never drop from the session's queue:
/// a promise waits on them (animation ends, measures, presentations),
/// a user action does (claims), or they happen once or carry state JS
/// keeps (an image's load, an exit's end, a layout, the window).
pub fn reliable(kind: u8) -> bool {
    matches!(
        kind,
        out_kind::ANIMATION_END
            | out_kind::CLAIM
            | out_kind::IMAGE
            | out_kind::EXIT_END
            | out_kind::LAYOUT
            | out_kind::MEASURE
            | out_kind::WINDOW
            | out_kind::PRESENTED
    )
}

/// `WINDOW` key bits.
pub mod window_bit {
    /// The window has keyboard focus.
    pub const FOCUSED: u32 = 1 << 0;
    /// The window shows: not minimized, not fully covered.
    pub const VISIBLE: u32 = 1 << 1;
    /// The system appearance is dark.
    pub const DARK: u32 = 1 << 2;
}

/// `PRESS` phases (key bits 4 and 5).
pub mod press_phase {
    /// The primary button went down on the pressable.
    pub const IN: u32 = 0;
    /// It came up (anywhere; an `ACTIVATE` follows when over the node).
    pub const OUT: u32 = 1;
    /// The press ended without a release: window focus lost, the node
    /// left the tree, or it became disabled.
    pub const CANCEL: u32 = 2;
}

/// What activated a pressable (`ACTIVATE` key bits 4 and 5).
pub mod activate_source {
    pub const POINTER: u32 = 0;
    /// Enter on key down or Space on key up.
    pub const KEY: u32 = 1;
    pub const ACCESSIBILITY: u32 = 2;
}

/// A key record's `key` field: the modifiers in bits 0 to 3 (as
/// pointer records), repeat in bit 4, composing in bit 5, the named key
/// (`Key::code`) in bits 8 to 15 and the physical key's US character
/// (`KeyInput::code`, ASCII; 0: none) in bits 16 to 23.
pub fn key_bits(k: &KeyInput, composing: bool) -> u32 {
    let code = k.code.filter(char::is_ascii).map_or(0, |c| c as u32);
    k.mods.bits() as u32
        | (k.repeat as u32) << 4
        | (composing as u32) << 5
        | k.key.code() << 8
        | code << 16
}

/// One event bound for JS: which node, what, pointer position (logical),
/// two float aux fields (button/wheel delta/scroll offset), a key code,
/// and an optional string payload.
#[derive(Clone, Debug)]
pub struct UiEvent {
    pub kind: u8,
    pub node: u32,
    /// The node's generation when the event fired. JS drops events whose
    /// generation no longer matches the id's current occupant.
    pub generation: u16,
    /// A text node's paragraph revision (`host::Paragraph::revision`)
    /// on pointer events that carry a span; 0 otherwise.
    pub revision: u32,
    pub x: f32,
    pub y: f32,
    pub a: f32,
    pub b: f32,
    pub key: u32,
    pub text: String,
}

impl UiEvent {
    pub fn new(kind: u8, node: u32) -> UiEvent {
        UiEvent {
            kind,
            node,
            generation: 0,
            revision: 0,
            x: 0.0,
            y: 0.0,
            a: 0.0,
            b: 0.0,
            key: 0,
            text: String::new(),
        }
    }
}

/// Listener mask bits — mirror `packages/bridge` EVENT_MASK. Stored in
/// `NodeProps::listeners`; dispatch only emits kinds a node asked for.
pub mod mask {
    pub const POINTER_MOVE: u32 = 1 << 0;
    pub const POINTER_DOWN: u32 = 1 << 1;
    pub const POINTER_UP: u32 = 1 << 2;
    pub const POINTER_ENTER_LEAVE: u32 = 1 << 3;
    pub const WHEEL: u32 = 1 << 4;
    pub const KEY: u32 = 1 << 5;
    pub const FOCUS: u32 = 1 << 6;
    pub const INPUT: u32 = 1 << 7;
    pub const SCROLL: u32 = 1 << 8;
    pub const PRESS: u32 = 1 << 9;
    pub const ACTIVATE: u32 = 1 << 10;
    pub const LAYOUT: u32 = 1 << 11;
}

/// Maps an outbound event kind to its listener mask bit.
pub fn mask_for(kind: u8) -> u32 {
    match kind {
        out_kind::POINTER_MOVE => mask::POINTER_MOVE,
        out_kind::POINTER_DOWN => mask::POINTER_DOWN,
        out_kind::POINTER_UP => mask::POINTER_UP,
        out_kind::POINTER_ENTER | out_kind::POINTER_LEAVE => mask::POINTER_ENTER_LEAVE,
        out_kind::WHEEL => mask::WHEEL,
        out_kind::KEY_DOWN | out_kind::KEY_UP => mask::KEY,
        out_kind::FOCUS | out_kind::BLUR => mask::FOCUS,
        out_kind::CHANGE | out_kind::SUBMIT => mask::INPUT,
        out_kind::SCROLL => mask::SCROLL,
        out_kind::PRESS => mask::PRESS,
        out_kind::ACTIVATE => mask::ACTIVATE,
        out_kind::LAYOUT => mask::LAYOUT,
        _ => 0,
    }
}

/// Serializes events into one outbox frame. Record layout (LE):
/// `kind u8 | pad u8 | generation u16 | node u32 | x f32 | y f32 | a f32
/// | b f32 | key u32 | revision u32 | text_len u32 | text utf8`. The frame starts with
/// a u32 record count.
pub fn encode_events(events: &[UiEvent]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + events.len() * 36);
    out.extend_from_slice(&(events.len() as u32).to_le_bytes());
    for e in events {
        out.extend_from_slice(&[e.kind, 0]);
        out.extend_from_slice(&e.generation.to_le_bytes());
        out.extend_from_slice(&e.node.to_le_bytes());
        for f in [e.x, e.y, e.a, e.b] {
            out.extend_from_slice(&f.to_le_bytes());
        }
        out.extend_from_slice(&e.key.to_le_bytes());
        out.extend_from_slice(&e.revision.to_le_bytes());
        out.extend_from_slice(&(e.text.len() as u32).to_le_bytes());
        out.extend_from_slice(e.text.as_bytes());
    }
    out
}
