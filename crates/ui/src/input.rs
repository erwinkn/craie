//! Native text input: a `PlainEditor` per INPUT-kind node plus undo and
//! clipboard glue. Key/pointer events reach here through `Ui::dispatch`;
//! text changes emit `onChangeText` events to the bridge.
//!
//! The editor owns the buffer — the JS `value` prop is a command
//! (`set_text`), not a per-keystroke round trip. Undo is a snapshot stack:
//! consecutive inserts coalesce into one entry, navigation/selection does
//! not record, and structural edits (delete, paste, undo) always split.

use std::collections::HashMap;

use crate::text::parley::style::StyleProperty;
use crate::text::parley::{self, Generation, PlainEditor};

use crate::clipboard::{Clipboard, MemoryClipboard};
use crate::geom::Size;
use crate::scene::PaintSlot;
use crate::text::TextEngine;

/// A snapshot of buffer + selection for undo.
#[derive(Clone)]
struct Snapshot {
    text: String,
    anchor: usize,
    focus: usize,
}

pub struct InputState {
    pub editor: PlainEditor<PaintSlot>,
    pub placeholder: String,
    pub font_size: f32,
    /// Text color, 0xRRGGBBAA — also the editor's default brush.
    pub color: u32,
    /// Selection fill, 0xRRGGBBAA.
    pub selection_color: u32,
    pub multiline: bool,
    /// Wrap width last handed to the editor; re-setting it is also how a
    /// style change marks the layout dirty (`PlainEditor` has no public
    /// invalidate).
    width: Option<f32>,
    /// Undo/redo snapshot stacks.
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Whether the last undo entry was produced by character insertion —
    /// consecutive inserts coalesce; anything else splits the entry.
    coalescing_insert: bool,
    /// Shaped placeholder and the wrap width it was shaped at. Dropped
    /// when the placeholder or the font size changes.
    pub placeholder_layout: Option<(u32, parley::Layout<PaintSlot>)>,
}

impl InputState {
    fn new(font_size: f32, color: u32, placeholder: String, multiline: bool) -> InputState {
        let mut editor = PlainEditor::new(font_size);
        // Slot 0 of the input chunk carries the text color.
        editor
            .edit_styles()
            .insert(StyleProperty::Brush(PaintSlot(0)));
        InputState {
            editor,
            placeholder,
            font_size,
            color,
            selection_color: 0x3584_E47A, // rgba(53,132,228,0.48)
            multiline,
            width: None,
            undo: Vec::new(),
            redo: Vec::new(),
            coalescing_insert: false,
            placeholder_layout: None,
        }
    }

    fn snapshot(&self) -> Snapshot {
        let sel = self.editor.raw_selection();
        Snapshot {
            text: self.editor.raw_text().to_string(),
            anchor: sel.anchor().index(),
            focus: sel.focus().index(),
        }
    }

    /// Sets the wrap width; unchanged widths leave the shaped buffer
    /// alone (`PlainEditor::set_width` always dirties it).
    pub fn set_width(&mut self, width: f32) {
        if self.width != Some(width) {
            self.width = Some(width);
            self.editor.set_width(Some(width));
        }
    }

    /// The editor's layout, reshaping only when dirty. Reshapes count in
    /// `TextEngine::shapes`.
    pub fn layout(&mut self, text: &mut TextEngine) -> &crate::text::parley::Layout<PaintSlot> {
        if self.editor.try_layout().is_none() {
            text.shapes += 1;
        }
        self.editor.layout(&mut text.font_cx, &mut text.layout_cx)
    }

    /// Marks the editor layout dirty (`set_width` is the public
    /// invalidation path; re-setting the same width still dirties).
    fn invalidate(&mut self) {
        self.editor.set_width(self.width);
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

    /// Restores a snapshot; returns true when something changed.
    fn restore(&mut self, text: &mut TextEngine, snap: Snapshot) {
        self.editor.set_text(&snap.text);
        let len = snap.text.len();
        // Reshape through the counted path; selecting then finds the
        // layout clean.
        self.layout(text);
        let mut drv = self.editor.driver(&mut text.font_cx, &mut text.layout_cx);
        drv.select_byte_range(snap.anchor.min(len), snap.focus.min(len));
    }
}

/// `InputState::record_undo` for use while a `PlainEditorDriver` holds
/// the editor borrow.
#[allow(clippy::too_many_arguments)]
fn record_undo(
    undo: &mut Vec<Snapshot>,
    redo: &mut Vec<Snapshot>,
    coalescing_insert: &mut bool,
    editor: &PlainEditor<PaintSlot>,
    coalesce: bool,
) {
    if coalesce && *coalescing_insert && !undo.is_empty() {
        return;
    }
    let sel = editor.raw_selection();
    undo.push(Snapshot {
        text: editor.raw_text().to_string(),
        anchor: sel.anchor().index(),
        focus: sel.focus().index(),
    });
    if undo.len() > 128 {
        undo.remove(0);
    }
    redo.clear();
    *coalescing_insert = coalesce;
}

/// All live text inputs, keyed by node id.
pub struct Inputs {
    map: HashMap<u32, InputState>,
    /// Buffer generation last reported to JS — `onChangeText` fires only
    /// when this differs.
    notified: HashMap<u32, Generation>,
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
                    state
                        .editor
                        .edit_styles()
                        .insert(StyleProperty::FontSize(font_size));
                }
                if state.placeholder != placeholder {
                    placeholder.clone_into(&mut state.placeholder);
                }
                state.multiline = multiline;
                state.placeholder_layout = None;
                state.invalidate();
                true
            }
            None => {
                self.map.insert(
                    id,
                    InputState::new(font_size, color, placeholder.to_string(), multiline),
                );
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
        Size::new(layout.width(), layout.height().max(line))
    }

    /// The committed text (preedit excluded) — the `value` seen by JS.
    pub fn text(&self, id: u32) -> String {
        self.map
            .get(&id)
            .map(|s| s.editor.text().to_string())
            .unwrap_or_default()
    }

    /// Replaces the whole buffer (JS `value` command).
    pub fn set_text(&mut self, id: u32, value: &str) {
        if let Some(state) = self.map.get_mut(&id) {
            state.record_undo(false);
            state.editor.set_text(value);
        }
    }

    /// Whether the buffer changed since JS was last notified; marks the
    /// current generation notified and returns the committed text.
    pub fn take_change(&mut self, id: u32) -> Option<String> {
        let state = self.map.get(&id)?;
        let generation = state.editor.generation();
        if self.notified.get(&id) == Some(&generation) {
            return None;
        }
        self.notified.insert(id, generation);
        Some(state.editor.text().to_string())
    }

    /// Applies an editing/pointer action to an input. Returns true when
    /// the buffer or selection changed enough to repaint; the caller
    /// separately asks `take_change` whether JS needs `onChangeText`.
    ///
    /// Reshapes count in `TextEngine::shapes` where they happen. Parley
    /// (pinned to 0.11.1) shapes in two places: `refresh_layout`, which
    /// navigation and deletion run when the layout is dirty, and
    /// `update_layout`, which every edit runs once when it changes the
    /// buffer (an insertion always does). Operations that read the
    /// layout get it clean through `InputState::layout` first (counted),
    /// so their own refresh does nothing; each edit then counts one.
    pub fn act(&mut self, text: &mut TextEngine, id: u32, action: &KeyAction) -> bool {
        // Undo/redo replace the buffer wholesale; they need the driver
        // after `set_text`, so handle them outside the driver scope.
        if matches!(action, KeyAction::Undo | KeyAction::Redo) {
            return self.undo_redo(text, id, matches!(action, KeyAction::Undo));
        }
        let Some(state) = self.map.get_mut(&id) else {
            return false;
        };
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
            state.layout(text);
        }
        let len_before = state.editor.raw_text().len();
        // Destructure so `record_undo` can touch the undo fields while
        // the driver holds `editor`.
        let InputState {
            editor,
            undo,
            redo,
            coalescing_insert,
            multiline,
            ..
        } = state;
        let mut drv = editor.driver(&mut text.font_cx, &mut text.layout_cx);
        let mut coalescing = false;
        // Edits that ran `update_layout`.
        let mut shaped = false;
        match action {
            KeyAction::Insert(s) => {
                record_undo(undo, redo, coalescing_insert, drv.editor, true);
                coalescing = true;
                shaped = true;
                if *multiline {
                    drv.insert_or_replace_selection(s);
                } else {
                    drv.insert_or_replace_selection(&s.replace(['\n', '\r'], ""));
                }
            }
            KeyAction::Newline => {
                if *multiline {
                    record_undo(undo, redo, coalescing_insert, drv.editor, false);
                    drv.insert_or_replace_selection("\n");
                    shaped = true;
                }
            }
            KeyAction::Backspace => {
                record_undo(undo, redo, coalescing_insert, drv.editor, false);
                drv.backdelete();
            }
            KeyAction::Delete => {
                record_undo(undo, redo, coalescing_insert, drv.editor, false);
                drv.delete();
            }
            KeyAction::BackspaceWord => {
                record_undo(undo, redo, coalescing_insert, drv.editor, false);
                drv.backdelete_word();
            }
            KeyAction::DeleteWord => {
                record_undo(undo, redo, coalescing_insert, drv.editor, false);
                drv.delete_word();
            }
            KeyAction::MoveLeft => drv.move_left(),
            KeyAction::MoveRight => drv.move_right(),
            KeyAction::MoveUp => drv.move_up(),
            KeyAction::MoveDown => drv.move_down(),
            KeyAction::MoveWordLeft => drv.move_word_left(),
            KeyAction::MoveWordRight => drv.move_word_right(),
            KeyAction::MoveLineStart => drv.move_to_line_start(),
            KeyAction::MoveLineEnd => drv.move_to_line_end(),
            KeyAction::MoveTextStart => drv.move_to_text_start(),
            KeyAction::MoveTextEnd => drv.move_to_text_end(),
            KeyAction::SelectLeft => drv.select_left(),
            KeyAction::SelectRight => drv.select_right(),
            KeyAction::SelectUp => drv.select_up(),
            KeyAction::SelectDown => drv.select_down(),
            KeyAction::SelectWordLeft => drv.select_word_left(),
            KeyAction::SelectWordRight => drv.select_word_right(),
            KeyAction::SelectLineStart => drv.select_to_line_start(),
            KeyAction::SelectLineEnd => drv.select_to_line_end(),
            KeyAction::SelectTextStart => drv.select_to_text_start(),
            KeyAction::SelectTextEnd => drv.select_to_text_end(),
            KeyAction::SelectAll => drv.select_all(),
            KeyAction::MoveTo(x, y) => drv.move_to_point(*x, *y),
            KeyAction::ExtendTo(x, y) => drv.extend_selection_to_point(*x, *y),
            KeyAction::SelectWordAt(x, y) => drv.select_word_at_point(*x, *y),
            KeyAction::Copy => {
                if let Some(sel) = drv.editor.selected_text() {
                    self.clipboard.set(sel);
                }
            }
            KeyAction::Cut => {
                if let Some(sel) = drv.editor.selected_text() {
                    self.clipboard.set(sel);
                    record_undo(undo, redo, coalescing_insert, drv.editor, false);
                    drv.delete_selection();
                    shaped = true;
                }
            }
            KeyAction::Paste => {
                if let Some(s) = self.clipboard.get()
                    && !s.is_empty()
                {
                    record_undo(undo, redo, coalescing_insert, drv.editor, false);
                    if *multiline {
                        drv.insert_or_replace_selection(&s);
                    } else {
                        drv.insert_or_replace_selection(&s.replace(['\n', '\r'], " "));
                    }
                    shaped = true;
                }
            }
            KeyAction::Submit => {}
            KeyAction::Undo | KeyAction::Redo => unreachable!(),
        }
        if !coalescing {
            *coalescing_insert = false;
        }
        drop(drv);
        // Deletions only remove bytes, and reshape exactly when they do.
        let deleted = state.editor.raw_text().len() < len_before;
        if matches!(
            action,
            KeyAction::Backspace
                | KeyAction::Delete
                | KeyAction::BackspaceWord
                | KeyAction::DeleteWord
        ) {
            shaped = deleted;
        }
        if shaped {
            text.shapes += 1;
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
        let composing = state.editor.is_composing();
        let mut drv = state.editor.driver(&mut text.font_cx, &mut text.layout_cx);
        if preedit.is_empty() {
            if composing {
                drv.clear_compose();
                text.shapes += 1;
            }
            return;
        }
        let cursor = cursor.map(|(a, b)| (a.min(preedit.len()), b.min(preedit.len())));
        drv.set_compose(preedit, cursor);
        text.shapes += 1;
    }

    /// IME commit: inserts `text` as committed input (one reshape).
    pub fn commit(&mut self, text: &mut TextEngine, id: u32, s: &str) {
        let Some(state) = self.map.get_mut(&id) else {
            return;
        };
        state.record_undo(false);
        let mut drv = state.editor.driver(&mut text.font_cx, &mut text.layout_cx);
        drv.insert_or_replace_selection(s);
        text.shapes += 1;
    }

    /// IME disabled / focus lost: keeps the composing text as committed
    /// text. Reshapes once when there was a composing region (its
    /// underline goes away).
    pub fn finish_compose(&mut self, text: &mut TextEngine, id: u32) {
        let Some(state) = self.map.get_mut(&id) else {
            return;
        };
        if !state.editor.is_composing() {
            return;
        }
        let mut drv = state.editor.driver(&mut text.font_cx, &mut text.layout_cx);
        drv.finish_compose();
        text.shapes += 1;
    }
}
