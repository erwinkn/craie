//! Event dispatch: hit testing, pointer capture, hover synthesis, focus
//! and Tab traversal, wheel scrolling, and input editing.

use std::time::Instant;

use crate::events::{self, Event, Key, KeyInput, Mods, UiEvent, mask, out_kind};
use craie_core::geom::{Affine, Point};

use crate::geom::Rect;
use crate::host::{NodeId, ROOT};
use crate::input::KeyAction;
use crate::mutation::NodeKind;
use crate::ui::Ui;

/// Whether `p` passes a clip: each bounded axis must contain it, and the
/// corners round only when both axes are bounded (as the shader does).
fn in_clip(p: Point, r: &Rect, radius: f32, open: [bool; 2]) -> bool {
    match open {
        [false, false] => in_rounded(p, r, radius),
        [true, false] => p.y >= r.origin.y && p.y < r.max_y(),
        [false, true] => p.x >= r.origin.x && p.x < r.max_x(),
        [true, true] => true,
    }
}

/// Whether `p` lies inside `r` with corners rounded by `radius` (the
/// same rounded-rect shape the renderer draws and clips with).
fn in_rounded(p: Point, r: &Rect, radius: f32) -> bool {
    if !r.contains(p) {
        return false;
    }
    let rad = radius.min(r.size.width / 2.0).min(r.size.height / 2.0);
    if rad <= 0.0 {
        return true;
    }
    let cx = p.x.clamp(r.origin.x + rad, r.max_x() - rad);
    let cy = p.y.clamp(r.origin.y + rad, r.max_y() - rad);
    let (dx, dy) = (p.x - cx, p.y - cy);
    dx * dx + dy * dy <= rad * rad
}

impl Ui {
    /// Ancestor chain of `id`, deepest first (inclusive).
    fn path_to(&self, id: NodeId) -> Vec<NodeId> {
        let mut path = Vec::new();
        let mut cur = id;
        loop {
            path.push(cur);
            let p = self.host.parent(cur);
            if !p.is_node() {
                break;
            }
            cur = p;
        }
        path
    }

    /// Emits `event` to every node on `path` whose listener mask covers
    /// its kind.
    fn emit_path(&mut self, path: &[NodeId], mut event: UiEvent) {
        let bit = events::mask_for(event.kind);
        for &id in path {
            if self.host.interaction(id).listeners & bit != 0 {
                event.node = id.0;
                event.generation = self.host.node(id).map_or(0, |n| n.generation);
                self.pending_events.push(event.clone());
            }
        }
    }

    /// Maps a window point (logical) into a node's content-box
    /// coordinates, undoing transforms and scroll offsets.
    pub(crate) fn to_content(&self, id: NodeId, x: f32, y: f32) -> (f32, f32) {
        let data = self.layouts.data(id);
        let p = self
            .node_to_window(id)
            .invert()
            .map_or(Point::new(f32::NAN, f32::NAN), |m| {
                m.apply(Point::new(x, y))
            });
        (p.x - data.content[0], p.y - data.content[1])
    }

    /// Deepest node containing (x, y) logical, honoring transforms, clip
    /// chains, scroll offsets, and `display: none`.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        let p = Point::new(x, y);
        for i in (0..self.host.child_count(ROOT)).rev() {
            let root = self.host.child_at(ROOT, i);
            if let Some(hit) = self.hit_node(root, p) {
                return Some(hit);
            }
        }
        None
    }

    /// `p` is in the parent's child frame (its border box minus scroll).
    /// Each node tests in its own frame, so clips and bounds stay exact
    /// under rotation and scale.
    fn hit_node(&self, id: NodeId, p: Point) -> Option<NodeId> {
        self.host.node(id)?;
        let style = self.host.style(id);
        if style.display == taffy::Display::None {
            return None;
        }
        let data = self.layouts.data(id);
        let size = data.rect.size;
        let mut q = Point::new(p.x - data.rect.origin.x, p.y - data.rect.origin.y);
        let t = self.host.spatial[id.index()].transform;
        if t != Affine::IDENTITY {
            let m = t.about(Point::new(size.width / 2.0, size.height / 2.0));
            q = m.invert()?.apply(q);
        }
        let clips = style.overflow.x != taffy::Overflow::Visible
            || style.overflow.y != taffy::Overflow::Visible;
        let (clip, radius, open) = self.clip_shape(id, [0.0, 0.0], &data);
        if !clips || in_clip(q, &clip, radius, open) {
            let [sx, sy] = self.scroll_offset_if_scrolls(id);
            let cp = Point::new(q.x + sx, q.y + sy);
            // Children paint above their parent and later siblings above
            // earlier ones: test them last to first.
            for &child in self.host.children(id).iter().rev() {
                if let Some(hit) = self.hit_node(child, cp) {
                    return Some(hit);
                }
            }
        }
        let own_radius = if self.host.kind(id).is_some_and(|k| k.has_box()) {
            self.host.paint[id.index()].radius
        } else {
            0.0
        };
        in_rounded(q, &Rect::new(0.0, 0.0, size.width, size.height), own_radius).then_some(id)
    }

    /// Focusable nodes in document order (for Tab traversal).
    fn focusables(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.host.children(ROOT).iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.host.node(id) else {
                continue;
            };
            if self.host.display_none(id) {
                continue;
            }
            if node.kind == NodeKind::Input || self.host.interaction(id).focusable {
                out.push(id);
            }
            for &child in self.host.children(id).iter().rev() {
                stack.push(child);
            }
        }
        out
    }

    /// Moves focus; emits blur/focus events, manages IME composition.
    pub fn set_focus(&mut self, next: Option<NodeId>) {
        if self.focus == next {
            return;
        }
        if let Some(old) = self.focus {
            if self.host.kind(old) == Some(NodeKind::Input) {
                self.inputs.finish_compose(&mut self.text, old.0);
                self.host.dirty.content.push(old.0);
            }
            if self.host.interaction(old).listeners & mask::FOCUS != 0 {
                self.pending_events.push(self.event(out_kind::BLUR, old));
            }
        }
        self.focus = next.filter(|&id| self.host.node(id).is_some());
        if let Some(id) = self.focus {
            if self.host.kind(id) == Some(NodeKind::Input) {
                self.host.dirty.content.push(id.0);
            }
            if self.host.interaction(id).listeners & mask::FOCUS != 0 {
                self.pending_events.push(self.event(out_kind::FOCUS, id));
            }
        }
        self.a11y_stale = true;
        self.force_paint = true;
    }

    /// The nearest ancestor of `id` (inclusive) whose style scrolls on
    /// the requested axis.
    fn scrollable_ancestor(&self, id: NodeId, dy: bool) -> Option<NodeId> {
        let mut cur = Some(id);
        while let Some(id) = cur {
            self.host.node(id)?;
            let style = self.host.style(id);
            let scrolls = if dy {
                style.overflow.y == taffy::Overflow::Scroll
            } else {
                style.overflow.x == taffy::Overflow::Scroll
            };
            if scrolls {
                return Some(id);
            }
            let p = self.host.parent(id);
            cur = p.is_node().then_some(p);
        }
        None
    }

    /// Handles one normalized platform event: native consumption
    /// (scroll, editing, focus) plus JS emission into `pending_events`.
    pub fn dispatch(&mut self, ev: &Event) {
        match ev {
            Event::PointerMove { x, y } => self.pointer_move(*x, *y),
            Event::PointerDown { x, y, button, mods } => self.pointer_down(*x, *y, *button, *mods),
            Event::PointerUp { x, y, button } => {
                // Pointer capture: while a button was held the press
                // target owns the release, wherever the pointer is.
                let target = self.pressed.take().or_else(|| self.hit_test(*x, *y));
                if let Some(hit) = target {
                    self.emit_pointer(hit, out_kind::POINTER_UP, *x, *y, *button, Mods::default());
                }
            }
            Event::Wheel { x, y, dx, dy } => {
                let hit = self.hit_test(*x, *y);
                if let Some(hit) = hit {
                    // Consume vertically or horizontally scrollable
                    // ancestors first.
                    let target = self
                        .scrollable_ancestor(hit, true)
                        .or_else(|| self.scrollable_ancestor(hit, false));
                    if let Some(id) = target
                        && let Some(off) = self.scroll_by(id, *dx, *dy)
                        && self.host.interaction(id).listeners & mask::SCROLL != 0
                    {
                        let mut e = self.event(out_kind::SCROLL, id);
                        e.a = off[0];
                        e.b = off[1];
                        self.pending_events.push(e);
                    }
                    let path = self.path_to(hit);
                    let mut e = self.event(out_kind::WHEEL, hit);
                    e.x = *x;
                    e.y = *y;
                    e.a = *dx;
                    e.b = *dy;
                    self.emit_path(&path, e);
                }
            }
            Event::KeyDown(k) => self.key_down(k),
            Event::KeyUp(k) => {
                if let Some(f) = self.focus {
                    let path = self.path_to(f);
                    let mut e = self.event(out_kind::KEY_UP, f);
                    e.key = k.key.code();
                    e.text = k.char.clone().unwrap_or_default();
                    self.emit_path(&path, e);
                }
            }
            Event::ImePreedit { text, cursor } => {
                if let Some(f) = self.focus {
                    self.inputs.set_compose(&mut self.text, f.0, text, *cursor);
                    self.input_changed(f);
                }
            }
            Event::ImeCommit(s) => {
                if let Some(f) = self.focus {
                    self.inputs.commit(&mut self.text, f.0, s);
                    self.input_changed(f);
                    self.emit_change(f);
                }
            }
            Event::ImeDone => {
                if let Some(f) = self.focus {
                    self.inputs.finish_compose(&mut self.text, f.0);
                    self.input_changed(f);
                }
            }
            Event::Focus(gained) => {
                if !gained {
                    self.hover = None;
                    self.pressed = None;
                }
            }
        }
    }

    fn pointer_move(&mut self, x: f32, y: f32) {
        // Drag capture: a press inside an input extends its selection.
        if let Some(pid) = self.pressed
            && self.host.kind(pid) == Some(NodeKind::Input)
        {
            let (lx, ly) = self.to_content(pid, x, y);
            self.inputs
                .act(&mut self.text, pid.0, &KeyAction::ExtendTo(lx, ly));
            self.input_changed(pid);
        }
        let hit = self.hit_test(x, y);
        if hit != self.hover {
            // Synthesize leave/enter on the symmetric difference of the
            // ancestor chains.
            let old: Vec<NodeId> = self.hover.map(|h| self.path_to(h)).unwrap_or_default();
            let new: Vec<NodeId> = hit.map(|h| self.path_to(h)).unwrap_or_default();
            for &id in &old {
                if !new.contains(&id)
                    && self.host.interaction(id).listeners & mask::POINTER_ENTER_LEAVE != 0
                {
                    let mut e = self.event(out_kind::POINTER_LEAVE, id);
                    e.x = x;
                    e.y = y;
                    self.pending_events.push(e);
                }
            }
            for &id in &new {
                if !old.contains(&id)
                    && self.host.interaction(id).listeners & mask::POINTER_ENTER_LEAVE != 0
                {
                    let mut e = self.event(out_kind::POINTER_ENTER, id);
                    e.x = x;
                    e.y = y;
                    self.pending_events.push(e);
                }
            }
            self.hover = hit;
        }
        // Pointer capture: a pressed node keeps receiving moves outside
        // its bounds (text selection drag, slider behaviors).
        if let Some(target) = self.pressed.or(hit) {
            self.emit_pointer(
                target,
                out_kind::POINTER_MOVE,
                x,
                y,
                crate::events::Button::Primary,
                Mods::default(),
            );
        }
    }

    fn pointer_down(&mut self, x: f32, y: f32, button: crate::events::Button, mods: Mods) {
        let hit = self.hit_test(x, y);
        self.pressed = hit;

        // Focus: nearest focusable/input ancestor of the hit; clicking
        // non-focusable space blurs.
        let focus_target = hit.and_then(|h| {
            self.path_to(h).into_iter().find(|&id| {
                self.host.kind(id) == Some(NodeKind::Input) || self.host.interaction(id).focusable
            })
        });
        self.set_focus(focus_target);

        // Input hit: caret/selection.
        if let Some(id) = hit
            && self.host.kind(id) == Some(NodeKind::Input)
            && button == crate::events::Button::Primary
        {
            let (lx, ly) = self.to_content(id, x, y);
            let action = if mods.shift {
                KeyAction::ExtendTo(lx, ly)
            } else {
                let double = self
                    .last_click
                    .map(|(t, n, px, py)| {
                        n == id
                            && t.elapsed().as_millis() < 500
                            && (px - x).abs() < 4.0
                            && (py - y).abs() < 4.0
                    })
                    .unwrap_or(false);
                if double {
                    KeyAction::SelectWordAt(lx, ly)
                } else {
                    KeyAction::MoveTo(lx, ly)
                }
            };
            self.last_click = Some((Instant::now(), id, x, y));
            self.inputs.act(&mut self.text, id.0, &action);
            self.input_changed(id);
        }
        if let Some(hit) = hit {
            self.emit_pointer(hit, out_kind::POINTER_DOWN, x, y, button, mods);
        }
    }

    fn emit_pointer(
        &mut self,
        hit: NodeId,
        kind: u8,
        x: f32,
        y: f32,
        button: crate::events::Button,
        mods: Mods,
    ) {
        let path = self.path_to(hit);
        let button_code = match button {
            crate::events::Button::Primary => 1,
            crate::events::Button::Secondary => 2,
            crate::events::Button::Middle => 3,
            crate::events::Button::Other(b) => b as u32,
        };
        let key = (mods.shift as u32)
            | ((mods.ctrl as u32) << 1)
            | ((mods.alt as u32) << 2)
            | ((mods.meta as u32) << 3)
            | (button_code << 8);
        // `a`/`b` carry coordinates relative to each receiving node's
        // border box — the offsets behaviors (sliders, drags) need.
        let bit = events::mask_for(kind);
        for &id in &path {
            if self.host.interaction(id).listeners & bit == 0 {
                continue;
            }
            let local = self
                .node_to_window(id)
                .invert()
                .map_or(Point::new(0.0, 0.0), |m| m.apply(Point::new(x, y)));
            let mut e = self.event(kind, id);
            e.x = x;
            e.y = y;
            e.a = local.x;
            e.b = local.y;
            e.key = key;
            self.pending_events.push(e);
        }
    }

    fn key_down(&mut self, k: &KeyInput) {
        // Tab traversal beats everything.
        if k.key == Key::Tab && !k.mods.ctrl && !k.mods.meta {
            let focusables = self.focusables();
            if !focusables.is_empty() {
                let next = match self.focus {
                    None => focusables[0],
                    Some(f) => {
                        let i = focusables.iter().position(|&n| n == f);
                        match (i, k.mods.shift) {
                            (Some(i), true) => {
                                focusables[(i + focusables.len() - 1) % focusables.len()]
                            }
                            (Some(i), false) => focusables[(i + 1) % focusables.len()],
                            (None, _) => focusables[0],
                        }
                    }
                };
                self.set_focus(Some(next));
            }
        }

        let focused_input = self
            .focus
            .filter(|&f| self.host.kind(f) == Some(NodeKind::Input));

        if let Some(id) = focused_input {
            if k.key == Key::Escape {
                self.set_focus(None);
            }
            let multiline = self.inputs.get(id.0).map(|s| s.multiline).unwrap_or(false);
            if let Some(action) = key_action(k, multiline) {
                match action {
                    KeyAction::Submit => {
                        let mut e = self.event(out_kind::SUBMIT, id);
                        e.text = self.inputs.text(id.0);
                        self.pending_events.push(e);
                    }
                    _ => {
                        self.inputs.act(&mut self.text, id.0, &action);
                        self.input_changed(id);
                        self.emit_change(id);
                    }
                }
            }
        }

        // Keys go to the focused node's path (or every KEY listener when
        // nothing is focused — global shortcuts).
        let mut e = UiEvent::new(out_kind::KEY_DOWN, 0);
        e.key = k.key.code();
        e.text = k.char.clone().unwrap_or_default();
        match self.focus {
            Some(f) => {
                let path = self.path_to(f);
                self.emit_path(&path, e);
            }
            None => {
                // Global delivery: every node with a key listener.
                let mut stack: Vec<NodeId> =
                    self.host.children(ROOT).iter().rev().copied().collect();
                while let Some(id) = stack.pop() {
                    if self.host.node(id).is_none() {
                        continue;
                    }
                    if self.host.interaction(id).listeners & mask::KEY != 0 {
                        e.node = id.0;
                        e.generation = self.host.node(id).map_or(0, |n| n.generation);
                        self.pending_events.push(e.clone());
                    }
                    for &child in self.host.children(id).iter().rev() {
                        stack.push(child);
                    }
                }
            }
        }
    }

    /// An input's buffer, caret, selection, or composition changed:
    /// relayout it and redraw its chunk.
    fn input_changed(&mut self, id: NodeId) {
        self.host.mark_layout(id);
        self.host.dirty.content.push(id.0);
        self.force_paint = true;
    }

    /// Emits `onChangeText` when the input buffer advanced.
    pub(crate) fn emit_change(&mut self, id: NodeId) {
        if let Some(text) = self.inputs.take_change(id.0) {
            self.a11y_stale = true;
            self.host.dirty.semantic.push(id.0);
            if self.host.interaction(id).listeners & mask::INPUT != 0 {
                let mut e = self.event(out_kind::CHANGE, id);
                e.text = text;
                self.pending_events.push(e);
            }
        }
    }

    /// The IME cursor area of the focused input, in logical points:
    /// absolute rect of the caret/composition region.
    pub fn ime_area(&self) -> Option<Rect> {
        let id = self.focus?;
        if self.host.kind(id) != Some(NodeKind::Input) {
            return None;
        }
        let state = self.inputs.get(id.0)?;
        let data = self.layouts.data(id);
        let area = state.editor.ime_cursor_area();
        Some(self.node_to_window(id).map_rect(&Rect::new(
            data.content[0] + area.x0 as f32,
            data.content[1] + area.y0 as f32,
            (area.x1 - area.x0).max(0.0) as f32,
            (area.y1 - area.y0).max(0.0) as f32,
        )))
    }

    /// Whether an input node is focused — the platform enables IME.
    pub fn ime_wanted(&self) -> bool {
        self.focus
            .map(|f| self.host.kind(f) == Some(NodeKind::Input))
            .unwrap_or(false)
    }
}

/// Maps a normalized key event to an editing action. `meta` (Cmd/Super)
/// takes priority, then `alt` (word granularity), then `shift`
/// (selection). Returns `None` when the key means nothing to an input.
fn key_action(k: &KeyInput, multiline: bool) -> Option<KeyAction> {
    let m = &k.mods;
    // Printable text inserts unless a command chord is held.
    if !m.meta
        && !m.ctrl
        && let Some(t) = &k.text
        && !t.is_empty()
    {
        return Some(KeyAction::Insert(t.clone()));
    }
    if m.meta || m.ctrl {
        let c = k.char.as_deref().unwrap_or("");
        return Some(match (c, k.key) {
            ("a", _) => KeyAction::SelectAll,
            ("c", _) => KeyAction::Copy,
            ("x", _) => KeyAction::Cut,
            ("v", _) => KeyAction::Paste,
            ("z", _) if m.shift => KeyAction::Redo,
            ("z", _) | ("y", _) => KeyAction::Undo,
            (_, Key::Left) if m.shift => KeyAction::SelectTextStart,
            (_, Key::Left) => KeyAction::MoveTextStart,
            (_, Key::Right) if m.shift => KeyAction::SelectTextEnd,
            (_, Key::Right) => KeyAction::MoveTextEnd,
            (_, Key::Up) if m.shift => KeyAction::SelectTextStart,
            (_, Key::Up) => KeyAction::MoveTextStart,
            (_, Key::Down) if m.shift => KeyAction::SelectTextEnd,
            (_, Key::Down) => KeyAction::MoveTextEnd,
            _ => return None,
        });
    }
    let sel = m.shift;
    Some(match k.key {
        Key::Backspace if m.alt => KeyAction::BackspaceWord,
        Key::Backspace => KeyAction::Backspace,
        Key::Delete if m.alt => KeyAction::DeleteWord,
        Key::Delete => KeyAction::Delete,
        Key::Enter => {
            if multiline {
                KeyAction::Newline
            } else {
                KeyAction::Submit
            }
        }
        Key::Escape => return None, // handled by the caller as blur intent
        Key::Tab => return None,    // traversal handled above
        Key::Left if sel && m.alt => KeyAction::SelectWordLeft,
        Key::Left if sel => KeyAction::SelectLeft,
        Key::Left if m.alt => KeyAction::MoveWordLeft,
        Key::Left => KeyAction::MoveLeft,
        Key::Right if sel && m.alt => KeyAction::SelectWordRight,
        Key::Right if sel => KeyAction::SelectRight,
        Key::Right if m.alt => KeyAction::MoveWordRight,
        Key::Right => KeyAction::MoveRight,
        Key::Up if sel => KeyAction::SelectUp,
        Key::Up => KeyAction::MoveUp,
        Key::Down if sel => KeyAction::SelectDown,
        Key::Down => KeyAction::MoveDown,
        Key::Home if sel => KeyAction::SelectLineStart,
        Key::Home => KeyAction::MoveLineStart,
        Key::End if sel => KeyAction::SelectLineEnd,
        Key::End => KeyAction::MoveLineEnd,
        Key::PageUp | Key::PageDown | Key::Unknown => return None,
    })
}
