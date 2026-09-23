//! Input events, normalized at the platform boundary.
//!
//! `platform/` produces `Event`s in logical points with winit types
//! resolved to Craie's small enums; `Ui::dispatch` consumes them. Events
//! the JS side subscribes to are encoded by `encode_events` into outbox
//! frames (`crate::bridge`).

/// Keyboard modifier state at event time.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Cmd on macOS, Super elsewhere.
    pub meta: bool,
}

/// Named (non-text) keys Craie recognizes. Code values are the wire u32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
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
        }
    }
}

/// A normalized key press/release.
#[derive(Clone, Debug)]
pub struct KeyInput {
    pub key: Key,
    /// Printable text for this press (layout + shift applied); `None`
    /// for pure named keys and when a command modifier is held.
    pub text: Option<String>,
    /// The raw character when `key` is `Key::Unknown` — e.g. "a" — so JS
    /// shortcuts can key on it.
    pub char: Option<String>,
    pub mods: Mods,
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
    PointerMove { x: f32, y: f32 },
    PointerDown { x: f32, y: f32, button: Button, mods: Mods },
    PointerUp { x: f32, y: f32, button: Button },
    /// dx/dy in logical points, sign = content scroll direction.
    Wheel { x: f32, y: f32, dx: f32, dy: f32 },
    KeyDown(KeyInput),
    KeyUp(KeyInput),
    /// IME composing region update; `None` cursor hides the caret.
    ImePreedit { text: String, cursor: Option<(usize, usize)> },
    ImeCommit(String),
    /// The platform reports composing is done.
    ImeDone,
    /// Window focus changed.
    Focus(bool),
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
    pub const KEY_DOWN: u8 = 7;
    pub const KEY_UP: u8 = 8;
    pub const FOCUS: u8 = 9;
    pub const BLUR: u8 = 10;
    /// Text input buffer changed; `text` carries the committed value.
    pub const CHANGE: u8 = 11;
    /// Enter pressed in a single-line input; `text` carries the value.
    pub const SUBMIT: u8 = 12;
    /// A scrollable node's offset changed natively; x/y = offset.
    pub const SCROLL: u8 = 13;
}

/// One event bound for JS: which node, what, pointer position (logical),
/// two float aux fields (button/wheel delta/scroll offset), a key code,
/// and an optional string payload.
#[derive(Clone, Debug)]
pub struct UiEvent {
    pub kind: u8,
    pub node: u32,
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
        _ => 0,
    }
}

/// Serializes events into one outbox frame. Record layout (LE):
/// `kind u8 | pad u8 | pad u16 | node u32 | x f32 | y f32 | a f32 | b f32
/// | key u32 | text_len u32 | text utf8`. The frame starts with a u32
/// record count.
pub fn encode_events(events: &[UiEvent]) -> Vec<u8> {
    let mut out = Vec::with_capacity(events.len() * 28);
    out.extend_from_slice(&(events.len() as u32).to_le_bytes());
    for e in events {
        out.extend_from_slice(&[e.kind, 0, 0, 0]);
        out.extend_from_slice(&e.node.to_le_bytes());
        for f in [e.x, e.y, e.a, e.b] {
            out.extend_from_slice(&f.to_le_bytes());
        }
        out.extend_from_slice(&e.key.to_le_bytes());
        out.extend_from_slice(&(e.text.len() as u32).to_le_bytes());
        out.extend_from_slice(e.text.as_bytes());
    }
    out
}
