//! Event dispatch: hit testing, pointer capture, hover synthesis, focus
//! and Tab traversal, wheel scrolling, and input editing.

use std::time::Instant;

use crate::events::{self, Event, Key, KeyInput, Mods, UiEvent, mask, out_kind};
use craie_core::geom::{Affine, Point};

use crate::geom::Rect;
use crate::host::{NodeFlags, NodeId, ROOT};
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
    // Not `clamp`: at a radius of half the size, rounding can put the
    // low bound a hair above the high one, and `clamp` panics then.
    let cx = p.x.max(r.origin.x + rad).min(r.max_x() - rad);
    let cy = p.y.max(r.origin.y + rad).min(r.max_y() - rad);
    let (dx, dy) = (p.x - cx, p.y - cy);
    dx * dx + dy * dy <= rad * rad
}

impl Ui {
    /// `id` and its ancestors, deepest first.
    pub(crate) fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(id), |&n| {
            Some(self.host.parent(n)).filter(|p| p.is_node())
        })
    }

    /// The deepest node on the ancestor chains of both `a` and `b`.
    fn common_ancestor(&self, a: Option<NodeId>, b: Option<NodeId>) -> Option<NodeId> {
        let (mut a, mut b) = (a?, b?);
        let (mut da, mut db) = (self.ancestors(a).count(), self.ancestors(b).count());
        while da > db {
            a = self.host.parent(a);
            da -= 1;
        }
        while db > da {
            b = self.host.parent(b);
            db -= 1;
        }
        while a != b {
            a = self.host.parent(a);
            b = self.host.parent(b);
        }
        a.is_node().then_some(a)
    }

    /// Emits `event` to `id` and each ancestor whose listener mask covers
    /// its kind.
    fn emit_path(&mut self, id: NodeId, mut event: UiEvent) {
        let bit = events::mask_for(event.kind);
        let mut cur = id;
        while cur.is_node() {
            if self.host.interaction(cur).listeners & bit != 0 {
                event.node = cur.0;
                event.generation = self.host.node(cur).map_or(0, |n| n.generation);
                self.pending_events.push(event.clone());
            }
            cur = self.host.parent(cur);
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
    /// chains, scroll offsets, and `display: none`. Skips the subtrees
    /// whose reach misses the point (`reach.rs`).
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        self.hit_roots(Point::new(x, y), true)
    }

    /// `hit_test` visiting every node: the oracle for the index.
    pub fn hit_test_walk(&self, x: f32, y: f32) -> Option<NodeId> {
        self.hit_roots(Point::new(x, y), false)
    }

    fn hit_roots(&self, p: Point, prune: bool) -> Option<NodeId> {
        for i in (0..self.host.child_count(ROOT)).rev() {
            let root = self.host.child_at(ROOT, i);
            if let Some(hit) = self.hit_node(root, p, prune) {
                return Some(hit);
            }
        }
        None
    }

    /// `p` is in the parent's child frame (its border box minus scroll).
    /// Each node tests in its own frame, so clips and bounds stay exact
    /// under rotation and scale.
    fn hit_node(&self, id: NodeId, p: Point, prune: bool) -> Option<NodeId> {
        let node = self.host.node(id)?;
        if prune
            && !node.flags.contains(NodeFlags::REACH)
            && self.reach.get(id.index()).is_some_and(|r| !r.contains(p))
        {
            return None;
        }
        let style = self.host.style(id);
        if style.display() == taffy::Display::None {
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
        let overflow = style.overflow();
        let clips =
            overflow.x != taffy::Overflow::Visible || overflow.y != taffy::Overflow::Visible;
        let (clip, radius, open) = self.clip_shape(id, [0.0, 0.0], &data);
        if !clips || in_clip(q, &clip, radius, open) {
            let [sx, sy] = self.scroll_offset_if_scrolls(id);
            let cp = Point::new(q.x + sx, q.y + sy);
            // Children paint above their parent and later siblings above
            // earlier ones: test them last to first.
            for &child in self.host.children(id).iter().rev() {
                if let Some(hit) = self.hit_node(child, cp, prune) {
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

    /// The span of text node `id` under window point (x, y).
    fn span_at(&self, id: NodeId, x: f32, y: f32) -> Option<u32> {
        if self.host.kind(id) != Some(NodeKind::Text) {
            return None;
        }
        let (lx, ly) = self.to_content(id, x, y);
        let cluster = self.text_layout(id)?.cluster_at_point(lx, ly)?;
        let spans = &self.host.paragraph(id)?.spans;
        let k = spans.partition_point(|s| s.start <= cluster.start);
        Some(k.saturating_sub(1) as u32)
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
                // A composition in progress becomes committed text.
                self.inputs.finish_compose(&mut self.text, old.0);
                self.host.dirty.content.push(old.0);
                self.emit_change(old);
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
            let overflow = style.overflow();
            let scrolls = if dy {
                overflow.y == taffy::Overflow::Scroll
            } else {
                overflow.x == taffy::Overflow::Scroll
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
        self.refresh_reach();
        match ev {
            Event::PointerMove { x, y } => self.pointer_move(*x, *y),
            Event::PointerDown { x, y, button, mods } => self.pointer_down(*x, *y, *button, *mods),
            Event::PointerUp { x, y, button } => {
                self.selecting = false;
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
                    let mut e = self.event(out_kind::WHEEL, hit);
                    e.x = *x;
                    e.y = *y;
                    e.a = *dx;
                    e.b = *dy;
                    self.emit_path(hit, e);
                }
            }
            Event::KeyDown(k) => self.key_down(k),
            Event::KeyUp(k) => {
                if let Some(f) = self.focus {
                    let mut e = self.event(out_kind::KEY_UP, f);
                    e.key = k.key.code();
                    e.text = k.char.clone().unwrap_or_default();
                    self.emit_path(f, e);
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
                    self.emit_change(f);
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
        if self.selecting {
            self.selection_drag(x, y);
        }
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
            // ancestor chains: each chain below their common ancestor.
            let common = self.common_ancestor(self.hover, hit).unwrap_or(ROOT);
            for (from, kind) in [
                (self.hover, out_kind::POINTER_LEAVE),
                (hit, out_kind::POINTER_ENTER),
            ] {
                let mut cur = from.unwrap_or(ROOT);
                while cur.is_node() && cur != common {
                    if self.host.interaction(cur).listeners & mask::POINTER_ENTER_LEAVE != 0 {
                        let mut e = self.event(kind, cur);
                        e.x = x;
                        e.y = y;
                        self.pending_events.push(e);
                    }
                    cur = self.host.parent(cur);
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
            self.ancestors(h).find(|&id| {
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
        // Text selection: a primary press outside inputs starts one in its
        // selectable domain, or clears one; a press in an input clears it
        // (the input keeps its own selection).
        let in_input = hit.is_some_and(|h| self.host.kind(h) == Some(NodeKind::Input));
        if button == crate::events::Button::Primary {
            if in_input {
                self.selecting = false;
                self.set_text_selection(None);
            } else {
                self.selecting = self.selection_press(hit, x, y, mods.shift);
            }
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
        let mut id = hit;
        while id.is_node() {
            if self.host.interaction(id).listeners & bit != 0 {
                // A text node's event carries the span under the pointer
                // (key bits 16+, span + 1; 0: none): nested Text routes
                // by it.
                let span = self.span_at(id, x, y).map_or(0, |s| s + 1);
                let local = self
                    .node_to_window(id)
                    .invert()
                    .map_or(Point::new(0.0, 0.0), |m| m.apply(Point::new(x, y)));
                let mut e = self.event(kind, id);
                e.x = x;
                e.y = y;
                e.a = local.x;
                e.b = local.y;
                e.key = key | span << 16;
                if span != 0 {
                    e.revision = self.host.paragraph(id).map_or(0, |p| p.revision);
                }
                self.pending_events.push(e);
            }
            id = self.host.parent(id);
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

        // Text selection keys, outside inputs: copy, select all, clear.
        if focused_input.is_none() && self.text_selection.is_some() {
            let command = k.mods.meta || k.mods.ctrl;
            match (command, k.char.as_deref(), k.key) {
                (true, Some("c"), _) => {
                    let text = self.selected_text();
                    if !text.is_empty() {
                        self.inputs.clipboard.set(&text);
                    }
                }
                (true, Some("a"), _) => self.select_domain(),
                (false, _, Key::Escape) => self.set_text_selection(None),
                _ => {}
            }
        }

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
            Some(f) => self.emit_path(f, e),
            None => {
                // Global delivery: every node with a key listener.
                let mut stack = std::mem::take(&mut self.walk);
                stack.clear();
                stack.extend(self.host.children(ROOT).iter().rev());
                while let Some(id) = stack.pop() {
                    if self.host.node(id).is_none() {
                        continue;
                    }
                    if self.host.interaction(id).listeners & mask::KEY != 0 {
                        e.node = id.0;
                        e.generation = self.host.node(id).map_or(0, |n| n.generation);
                        self.pending_events.push(e.clone());
                    }
                    stack.extend(self.host.children(id).iter().rev());
                }
                self.walk = stack;
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
        let (x, y, w, h) = state.editor.ime_area();
        Some(self.node_to_window(id).map_rect(&Rect::new(
            data.content[0] + x,
            data.content[1] + y,
            w.max(0.0),
            h.max(0.0),
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
