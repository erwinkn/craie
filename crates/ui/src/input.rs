//! Native text input: an owned `Editor` (craie-text) per INPUT-kind node
//! plus undo and clipboard glue. Key/pointer events reach here through `Ui::dispatch`;
//! text changes emit `onChangeText` events to the bridge.
//!
//! The editor owns the buffer — the JS `value` prop is a command
//! (`set_text`), not a per-keystroke round trip. Undo is a snapshot stack:
//! consecutive inserts coalesce into one entry, navigation/selection does
//! not record, and structural edits (delete, paste, undo) always split.

use std::collections::HashMap;

use crate::text::editor::{Cursor, Editor, Motion};
use crate::text::paragraph::Paragraph;

use crate::clipboard::{Clipboard, MemoryClipboard};
use crate::geom::Size;
use crate::text::TextEngine;

/// A snapshot of buffer + selection (full cursors, affinity included)
/// for undo.
#[derive(Clone)]
struct Snapshot {
    text: String,
    anchor: Cursor,
    focus: Cursor,
}

pub struct InputState {
    pub editor: Editor,
    pub placeholder: String,
    pub font_size: f32,
    /// Text color, 0xRRGGBBAA: the input chunk's paint slot 2.
    pub color: u32,
    /// Selection fill, 0xRRGGBBAA.
    pub selection_color: u32,
    pub multiline: bool,
    /// Undo/redo snapshot stacks.
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Whether the last undo entry was produced by character insertion —
    /// consecutive inserts coalesce; anything else splits the entry.
    coalescing_insert: bool,
    /// An undo entry was recorded when the current composition started:
    /// preedit updates, a clear before commit, and the commit are one
    /// undo step with it.
    compose_undo: bool,
    /// Shaped placeholder and the wrap width it is laid out at. Dropped
    /// when the placeholder or the font size changes; a width change
    /// rewraps it.
    pub placeholder_layout: Option<(u32, Paragraph)>,
}

impl InputState {
    fn new(font_size: f32, color: u32, placeholder: String, multiline: bool) -> InputState {
        InputState {
            editor: Editor::new(font_size),
            placeholder,
            font_size,
            color,
            selection_color: 0x3584_E47A, // rgba(53,132,228,0.48)
            multiline,
            undo: Vec::new(),
            redo: Vec::new(),
            coalescing_insert: false,
            compose_undo: false,
            placeholder_layout: None,
        }
    }

    fn snapshot(&self) -> Snapshot {
        let sel = self.editor.selection();
        Snapshot {
            text: self.editor.raw_text().to_string(),
            anchor: sel.anchor,
            focus: sel.focus,
        }
    }

    /// Sets the wrap width: the editor rewraps, it does not reshape.
    pub fn set_width(&mut self, width: f32) {
        self.editor.set_width(Some(width));
    }

    /// The editor's layout, brought up to date (shaping only after an
    /// edit; `TextEngine::shapes` counts it).
    pub fn layout(&mut self, text: &mut TextEngine) -> &Paragraph {
        self.editor.refresh(text);
        self.editor.layout()
    }

    /// Records an undo step before a mutating edit. `coalesce` merges
    /// consecutive steps (plain typing); non-coalescing edits split.
    fn record_undo(&mut self, coalesce: bool) {
        if coalesce && self.coalescing_insert && !self.undo.is_empty() {
            return;
        }
        self.undo.push(self.snapshot());
        if self.undo.len() > 128 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.coalescing_insert = coalesce;
    }

    /// Restores a snapshot (one reshape), cursors with their affinity.
    fn restore(&mut self, text: &mut TextEngine, snap: Snapshot) {
        self.editor.set_text(&snap.text);
        self.editor.refresh(text);
        self.editor.set_selection_cursors(snap.anchor, snap.focus);
    }
}

/// All live text inputs, keyed by node id.
pub struct Inputs {
    map: HashMap<u32, InputState>,
    /// Committed text last reported to JS (or set by it): `onChangeText`
    /// fires only when the committed text differs.
    notified: HashMap<u32, String>,
    /// Copy/cut/paste target. The platform installs the system clipboard.
    pub clipboard: Box<dyn Clipboard>,
}

impl Default for Inputs {
    fn default() -> Inputs {
        Inputs {
            map: HashMap::new(),
            notified: HashMap::new(),
            clipboard: Box::new(MemoryClipboard::default()),
        }
    }
}

/// A resolved editing/pointer action for the focused input. `dispatch`
/// turns platform events into these; `Inputs::act` performs them.
pub enum KeyAction {
    Insert(String),
    Backspace,
    Delete,
    BackspaceWord,
    DeleteWord,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveWordLeft,
    MoveWordRight,
    MoveLineStart,
    MoveLineEnd,
    MoveTextStart,
    MoveTextEnd,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectWordLeft,
    SelectWordRight,
    SelectLineStart,
    SelectLineEnd,
    SelectTextStart,
    SelectTextEnd,
    SelectAll,
    Newline,
    /// Enter on a single-line input: no edit, `dispatch` turns it into
    /// an `onSubmit` event.
    Submit,
    Cut,
    Copy,
    Paste,
    Undo,
    Redo,
    /// Place the caret at a click point (content-box relative, logical).
    MoveTo(f32, f32),
    /// Extend the selection to a drag point.
    ExtendTo(f32, f32),
    /// Double-click: select the word at a point.
    SelectWordAt(f32, f32),
}

impl Inputs {
    pub fn get(&self, id: u32) -> Option<&InputState> {
        self.map.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut InputState> {
        self.map.get_mut(&id)
    }

    /// Creates or reconfigures an input node.
    /// Returns whether the change affects layout (size, placeholder,
    /// wrapping); a color-only change does not.
    pub fn configure(
        &mut self,
        id: u32,
        font_size: f32,
        color: u32,
        placeholder: &str,
        multiline: bool,
    ) -> bool {
        match self.map.get_mut(&id) {
            Some(state) => {
                // The color lives in the chunk's paint record, not in
                // the editor's layout.
                state.color = color;
                let metrics = state.font_size != font_size
                    || state.placeholder != placeholder
                    || state.multiline != multiline;
                if !metrics {
                    return false;
                }
                if state.font_size != font_size {
                    state.font_size = font_size;
                    state.editor.set_font_size(font_size);
                }
                if state.placeholder != placeholder {
                    placeholder.clone_into(&mut state.placeholder);
                }
                state.multiline = multiline;
                state.placeholder_layout = None;
                true
            }
            None => {
                self.map.insert(
                    id,
                    InputState::new(font_size, color, placeholder.to_string(), multiline),
                );
                self.notified.insert(id, String::new());
                true
            }
        }
    }

    pub fn remove(&mut self, id: u32) {
        self.map.remove(&id);
        self.notified.remove(&id);
    }

    /// Intrinsic content size of an input at `width` (logical points).
    /// Single-line inputs measure one line tall; multiline inputs measure
    /// their laid-out height.
    pub fn measure(&mut self, text: &mut TextEngine, id: u32, width: f32) -> Size {
        let Some(state) = self.map.get_mut(&id) else {
            return Size::ZERO;
        };
        let width = width.max(0.0);
        state.set_width(width);
        let line = state.font_size * 1.25;
        let layout = state.layout(text);
        Size::new(layout.width, layout.height.max(line))
    }

    /// The committed text (preedit excluded) — the `value` seen by JS.
    pub fn text(&self, id: u32) -> String {
        self.map
            .get(&id)
            .map(|s| s.editor.text())
            .unwrap_or_default()
    }

    /// Replaces the whole buffer (JS `value` command).
    pub fn set_text(&mut self, id: u32, value: &str) {
        if let Some(state) = self.map.get_mut(&id) {
            state.record_undo(false);
            state.editor.set_text(value);
        }
    }

    /// Whether the committed text changed since JS was last notified;
    /// marks it notified and returns it. Selection moves, layout, and the
    /// preedit alone never count.
    pub fn take_change(&mut self, id: u32) -> Option<String> {
        let state = self.map.get(&id)?;
        if self
            .notified
            .get(&id)
            .is_some_and(|t| state.editor.text_is(t))
        {
            return None;
        }
        let text = state.editor.text();
        self.notified.insert(id, text.clone());
        Some(text)
    }

    /// Marks the current committed text as known to JS (after a `setText`
    /// command, which JS issued: no echo).
    pub fn mark_notified(&mut self, id: u32) {
        if let Some(state) = self.map.get(&id) {
            self.notified.insert(id, state.editor.text());
        }
    }

    /// Applies an editing/pointer action to an input. Returns true when
    /// the buffer or selection changed enough to repaint; the caller
    /// separately asks `take_change` whether JS needs `onChangeText`.
    ///
    /// The editor shapes where it must and `TextEngine::shapes` counts
    /// it: each edit that changes the buffer shapes once; actions that
    /// read the layout bring it up to date first (a reshape only when an
    /// earlier change left it dirty; a width change only rewraps).
    pub fn act(&mut self, text: &mut TextEngine, id: u32, action: &KeyAction) -> bool {
        if matches!(action, KeyAction::Undo | KeyAction::Redo) {
            return self.undo_redo(text, id, matches!(action, KeyAction::Undo));
        }
        let Some(state) = self.map.get_mut(&id) else {
            return false;
        };
        // A key action ends any composition's undo grouping.
        state.compose_undo = false;
        // Insertions and cuts only replace the selection: they read no
        // layout, and their reshape makes it clean.
        let reads_layout = !matches!(
            action,
            KeyAction::Insert(_)
                | KeyAction::Newline
                | KeyAction::Paste
                | KeyAction::Cut
                | KeyAction::Copy
                | KeyAction::Submit
        );
        if reads_layout {
            state.editor.refresh(text);
        }
        let mut coalescing = false;
        let ed = &mut state.editor;
        match action {
            KeyAction::Insert(s) => {
                state.record_undo(true);
                coalescing = true;
                let s = if state.multiline {
                    s.clone()
                } else {
                    s.replace(['\n', '\r'], "")
                };
                state.editor.insert_or_replace_selection(text, &s);
            }
            KeyAction::Newline => {
                if state.multiline {
                    state.record_undo(false);
                    state.editor.insert_or_replace_selection(text, "\n");
                }
            }
            KeyAction::Backspace => {
                state.record_undo(false);
                state.editor.backdelete(text);
            }
            KeyAction::Delete => {
                state.record_undo(false);
                state.editor.delete(text);
            }
            KeyAction::BackspaceWord => {
                state.record_undo(false);
                state.editor.backdelete_word(text);
            }
            KeyAction::DeleteWord => {
                state.record_undo(false);
                state.editor.delete_word(text);
            }
            KeyAction::MoveLeft => ed.motion(Motion::Left, false),
            KeyAction::MoveRight => ed.motion(Motion::Right, false),
            KeyAction::MoveUp => ed.motion(Motion::Up, false),
            KeyAction::MoveDown => ed.motion(Motion::Down, false),
            KeyAction::MoveWordLeft => ed.motion(Motion::WordLeft, false),
            KeyAction::MoveWordRight => ed.motion(Motion::WordRight, false),
            KeyAction::MoveLineStart => ed.motion(Motion::LineStart, false),
            KeyAction::MoveLineEnd => ed.motion(Motion::LineEnd, false),
            KeyAction::MoveTextStart => ed.motion(Motion::TextStart, false),
            KeyAction::MoveTextEnd => ed.motion(Motion::TextEnd, false),
            KeyAction::SelectLeft => ed.motion(Motion::Left, true),
            KeyAction::SelectRight => ed.motion(Motion::Right, true),
            KeyAction::SelectUp => ed.motion(Motion::Up, true),
            KeyAction::SelectDown => ed.motion(Motion::Down, true),
            KeyAction::SelectWordLeft => ed.motion(Motion::WordLeft, true),
            KeyAction::SelectWordRight => ed.motion(Motion::WordRight, true),
            KeyAction::SelectLineStart => ed.motion(Motion::LineStart, true),
            KeyAction::SelectLineEnd => ed.motion(Motion::LineEnd, true),
            KeyAction::SelectTextStart => ed.motion(Motion::TextStart, true),
            KeyAction::SelectTextEnd => ed.motion(Motion::TextEnd, true),
            KeyAction::SelectAll => ed.select_all(),
            KeyAction::MoveTo(x, y) => ed.move_to_point(*x, *y),
            KeyAction::ExtendTo(x, y) => ed.extend_to_point(*x, *y),
            KeyAction::SelectWordAt(x, y) => ed.select_word_at_point(*x, *y),
            KeyAction::Copy => {
                if let Some(sel) = ed.selected_text() {
                    self.clipboard.set(sel);
                }
            }
            KeyAction::Cut => {
                if let Some(sel) = ed.selected_text() {
                    self.clipboard.set(sel);
                    state.record_undo(false);
                    state.editor.delete_selection(text);
                }
            }
            KeyAction::Paste => {
                if let Some(s) = self.clipboard.get()
                    && !s.is_empty()
                {
                    state.record_undo(false);
                    let s = if state.multiline {
                        s
                    } else {
                        s.replace(['\n', '\r'], " ")
                    };
                    state.editor.insert_or_replace_selection(text, &s);
                }
            }
            KeyAction::Submit => {}
            KeyAction::Undo | KeyAction::Redo => unreachable!(),
        }
        if !coalescing {
            state.coalescing_insert = false;
        }
        true
    }

    fn undo_redo(&mut self, text: &mut TextEngine, id: u32, undo: bool) -> bool {
        let Some(state) = self.map.get_mut(&id) else {
            return false;
        };
        let snap = if undo {
            state.undo.pop()
        } else {
            state.redo.pop()
        };
        let Some(snap) = snap else { return false };
        let current = state.snapshot();
        if undo {
            state.redo.push(current);
        } else {
            state.undo.push(current);
        }
        state.coalescing_insert = false;
        state.restore(text, snap);
        true
    }

    /// IME composing text; `cursor` is a byte range within `text`.
    /// Replaces the preedit (or the selection) and reshapes once. An
    /// empty preedit (platforms send one before a commit) removes the
    /// composing text, reshaping once when there was some.
    pub fn set_compose(
        &mut self,
        text: &mut TextEngine,
        id: u32,
        preedit: &str,
        cursor: Option<(usize, usize)>,
    ) {
        let Some(state) = self.map.get_mut(&id) else {
            return;
        };
        if preedit.is_empty() {
            state.editor.clear_compose(text);
            return;
        }
        if !state.editor.is_composing() {
            // The composition replaces the selection: record it now; the
            // commit joins this entry.
            state.record_undo(false);
            state.compose_undo = true;
        }
        let n = preedit.len();
        let cursor = cursor.map(|(a, b)| (a.min(n) as u32, b.min(n) as u32));
        state.editor.set_compose(text, preedit, cursor);
    }

    /// IME commit: inserts `text` as committed input (one reshape).
    pub fn commit(&mut self, text: &mut TextEngine, id: u32, s: &str) {
        let Some(state) = self.map.get_mut(&id) else {
            return;
        };
        if !std::mem::take(&mut state.compose_undo) {
            state.record_undo(false);
        }
        state.editor.insert_or_replace_selection(text, s);
    }

    /// IME disabled / focus lost: keeps the composing text as committed
    /// text. Reshapes once when there was a composing region (its
    /// underline goes away).
    pub fn finish_compose(&mut self, text: &mut TextEngine, id: u32) {
        if let Some(state) = self.map.get_mut(&id) {
            state.compose_undo = false;
            state.editor.finish_compose(text);
        }
    }
}
