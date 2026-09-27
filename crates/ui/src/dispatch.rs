//! Event dispatch: hit testing, pointer capture, hover synthesis, focus
//! and Tab traversal, wheel scrolling, and input editing.

use std::time::Instant;

use crate::claims::{KeyMatch, claim_kind};
use crate::events::{self, Event, Key, KeyInput, Mods, UiEvent, mask, out_kind};
use craie_core::geom::{Affine, Point};

use crate::geom::Rect;
use crate::host::{NodeFlags, NodeId, ROOT};
use crate::input::{KeyAction, SubmitKey};
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
    /// whose reach misses the point (`reach.rs`). Allocation-free once
    /// paint orders are fresh (after `render` or within `dispatch`);
    /// between a transaction and those, a stale parent sorts on the spot.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        self.hit_roots(Point::new(x, y), true)
    }

    /// `hit_test` visiting every node: the oracle for the index.
    pub fn hit_test_walk(&self, x: f32, y: f32) -> Option<NodeId> {
        self.hit_roots(Point::new(x, y), false)
    }

    fn hit_roots(&self, p: Point, prune: bool) -> Option<NodeId> {
        for &root in self.host.paint_order(ROOT).iter().rev() {
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
            // Children paint above their parent, in paint order
            // (`order.rs`): test them last to first.
            for &child in self.host.paint_order(id).iter().rev() {
                if let Some(hit) = self.hit_node(child, cp, prune) {
                    return Some(hit);
                }
            }
        }
        // A layer container passes through (`box-none`).
        if node.flags.contains(NodeFlags::LAYER) {
            return None;
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
        self.host.refresh_orders();
        self.refresh_reach();
        match ev {
            Event::PointerMove { x, y } => {
                self.last_pointer = Some((*x, *y));
                self.pointer_move(*x, *y)
            }
            Event::PointerDown { x, y, button, mods } => {
                self.last_pointer = Some((*x, *y));
                self.states.keyboard = false;
                self.pointer_down(*x, *y, *button, *mods)
            }
            Event::PointerUp { x, y, button } => {
                self.selecting = false;
                self.pressed_primary = false;
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
            Event::KeyDown(k) => {
                self.states.keyboard = true;
                self.key_down(k)
            }
            Event::KeyUp(k) => {
                if let Some(f) = self.focus {
                    let composing = self
                        .inputs
                        .get(f.0)
                        .is_some_and(|s| s.editor.is_composing());
                    let mut e = self.event(out_kind::KEY_UP, f);
                    e.key = events::key_bits(k, composing);
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
            // Claimed on the path under the drop, or on the focus path
            // when the position is unknown (off the window: see
            // `Event::Drop`). Paths can hold newlines but not NUL.
            Event::Drop { x, y, paths } => {
                let hit = self.hit_test(*x, *y).or(self.focus);
                self.pointer_claim(hit, claim_kind::DROP, *x, *y, paths.join("\0"));
            }
            Event::Focus(gained) => {
                if !gained {
                    self.hover = None;
                    self.pressed = None;
                    self.pressed_primary = false;
                    self.last_pointer = None;
                }
            }
        }
        self.restyle();
    }

    /// Moves the hover to `hit`, synthesizing leave and enter on the
    /// symmetric difference of the ancestor chains: each chain below
    /// their common ancestor.
    fn set_hover(&mut self, hit: Option<NodeId>, x: f32, y: f32) {
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

    /// Hover at rest: after a frame whose geometry moved, the node under
    /// a still pointer may have changed. Held while a space is moving
    /// (a scroll, a transform): `settle` applies it. Returns whether the
    /// hover changed.
    pub(crate) fn rehover(&mut self) -> bool {
        let Some((x, y)) = self.last_pointer else {
            self.hover_stale = false;
            return false;
        };
        if self.next_settle().is_some() {
            self.hover_stale = true;
            return false;
        }
        self.hover_stale = false;
        self.refresh_reach();
        let hit = self.hit_test(x, y);
        if hit == self.hover {
            return false;
        }
        self.set_hover(hit, x, y);
        self.restyle();
        true
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
            self.set_hover(hit, x, y);
        }
        self.hover_stale = false;
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
        self.pressed_primary = button == crate::events::Button::Primary;

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
        if button == crate::events::Button::Secondary {
            self.pointer_claim(hit, claim_kind::CONTEXT_MENU, x, y, String::new());
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
        let key = mods.bits() as u32 | button_code << 8;
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
        let focused_input = self
            .focus
            .filter(|&f| self.host.kind(f) == Some(NodeKind::Input));
        let composing = focused_input.is_some_and(|f| {
            self.inputs
                .get(f.0)
                .is_some_and(|s| s.editor.is_composing())
        });

        // Claims beat every default. None match while an IME composes.
        if !composing {
            if let Some((node, found, version)) = self.key_claim(k, focused_input.is_some()) {
                if let KeyMatch::Claim(i) = found {
                    let e = self.claim_event(node, claim_kind::KEY, i, version);
                    self.pending_events.push(e);
                }
                return;
            }
            if self.clipboard_claim(k, focused_input) || self.context_menu_key(k) {
                return;
            }
        }

        // Tab traversal.
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

        // Text selection keys, outside inputs: copy, select all, clear.
        if focused_input.is_none() && self.text_selection.is_some() {
            if k.is(Mods::COMMAND, 'c') {
                let text = self.selected_text();
                if !text.is_empty() {
                    self.inputs.clipboard.set(&text);
                }
            } else if k.is(Mods::COMMAND, 'a') {
                self.select_domain();
            } else if k.key == Key::Escape && k.mods == Mods::default() {
                self.set_text_selection(None);
            }
        }

        if let Some(id) = focused_input
            && let Some(state) = self.inputs.get(id.0)
            && let Some(action) = key_action(k, state.multiline, state.submit)
        {
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

        // Unclaimed keys go to the focused node's path; with nothing
        // focused, to no one (window shortcuts are claims).
        if let Some(f) = self.focus {
            let mut e = self.event(out_kind::KEY_DOWN, f);
            e.key = events::key_bits(k, composing);
            e.text = k.char.clone().unwrap_or_default();
            self.emit_path(f, e);
        }
    }

    /// The key claim a press finds: the first match on the focused node
    /// and its ancestors, then on the window list (only claims allowed
    /// in inputs while one has focus). The claimer is NIL for the window.
    fn key_claim(&self, k: &KeyInput, in_input: bool) -> Option<(NodeId, KeyMatch, u32)> {
        if self.host.claims.is_empty() {
            return None;
        }
        let on_path = self.focus.and_then(|f| {
            self.ancestors(f).find_map(|n| {
                let set = self.host.claims.get(&n.0)?;
                Some((n, set.key(k, false)?, set.version))
            })
        });
        on_path.or_else(|| {
            let set = self.host.claims.get(&NodeId::NIL.0)?;
            Some((NodeId::NIL, set.key(k, in_input)?, set.version))
        })
    }

    /// The first claim of `kind` on `from` and its ancestors: the
    /// claimer, the claim's index, and its set's version.
    fn path_claim(&self, from: Option<NodeId>, kind: u8) -> Option<(NodeId, usize, u32)> {
        if self.host.claims.is_empty() {
            return None;
        }
        self.ancestors(from?).find_map(|n| {
            let set = self.host.claims.get(&n.0)?;
            Some((n, set.find(kind)?, set.version))
        })
    }

    /// A `CLAIM` event: claim `index` (of `kind`) of `node`'s set, as of
    /// `version`.
    fn claim_event(&self, node: NodeId, kind: u8, index: usize, version: u32) -> UiEvent {
        let mut e = self.event(out_kind::CLAIM, node);
        e.key = kind as u32 | (index as u32) << 8;
        e.revision = version;
        e
    }

    /// Mod+C, X or V, claimed on the focus path (with nothing focused,
    /// the text selection's domain and its ancestors): a claim event
    /// with the clipboard's text (paste) or the selected text (copy,
    /// cut) instead of the default. The answer comes back as
    /// `InsertText` and `WriteClipboard` commands.
    fn clipboard_claim(&mut self, k: &KeyInput, input: Option<NodeId>) -> bool {
        let kind = if k.is(Mods::COMMAND, 'c') {
            claim_kind::COPY
        } else if k.is(Mods::COMMAND, 'x') {
            claim_kind::CUT
        } else if k.is(Mods::COMMAND, 'v') {
            claim_kind::PASTE
        } else {
            return false;
        };
        let from = self.focus.or_else(|| self.text_selection.map(|s| s.domain));
        let Some((node, i, version)) = self.path_claim(from, kind) else {
            return false;
        };
        let mut e = self.claim_event(node, kind, i, version);
        e.text = match (kind, input) {
            (claim_kind::PASTE, _) => self.inputs.clipboard.get().unwrap_or_default(),
            (_, Some(id)) => self.inputs.selected_text(id.0).unwrap_or_default(),
            (_, None) => self.selected_text(),
        };
        self.pending_events.push(e);
        true
    }

    /// The ContextMenu key or Shift+F10, claimed on the focus path: a
    /// claim event at the focused node's center.
    fn context_menu_key(&mut self, k: &KeyInput) -> bool {
        let menu = (k.key == Key::ContextMenu && k.mods == Mods::default())
            || (k.key == Key::F(10) && k.mods.bits() == Mods::SHIFT);
        if !menu {
            return false;
        }
        let Some((node, i, version)) = self.path_claim(self.focus, claim_kind::CONTEXT_MENU) else {
            return false;
        };
        let focus = self.focus.unwrap_or(node);
        let size = self.layouts.data(focus).rect.size;
        let c = self
            .node_to_window(focus)
            .apply(Point::new(size.width / 2.0, size.height / 2.0));
        let mut e = self.claim_event(node, claim_kind::CONTEXT_MENU, i, version);
        e.x = c.x;
        e.y = c.y;
        self.pending_events.push(e);
        true
    }

    /// A pointer's claims: a secondary press claimed as a context menu,
    /// or files dropped, on the path under the pointer.
    fn pointer_claim(&mut self, hit: Option<NodeId>, kind: u8, x: f32, y: f32, text: String) {
        if let Some((node, i, version)) = self.path_claim(hit, kind) {
            let mut e = self.claim_event(node, kind, i, version);
            e.x = x;
            e.y = y;
            e.text = text;
            self.pending_events.push(e);
        }
    }

    /// An input's buffer, caret, selection, or composition changed:
    /// relayout it and redraw its chunk.
    pub(crate) fn input_changed(&mut self, id: NodeId) {
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

/// Maps a normalized key event to an editing action. Command chords
/// (`mod+c`, exact modifiers, `KeyInput::is`) come first, then `alt`
/// (word granularity), then `shift` (selection). Returns `None` when the
/// key means nothing to an input.
fn key_action(k: &KeyInput, multiline: bool, submit: SubmitKey) -> Option<KeyAction> {
    let m = &k.mods;
    // Enter per the submit key, with exact modifiers (Marbre's rule).
    // Shift+Enter is a newline in a multiline input.
    if k.key == Key::Enter {
        let (plain, shift) = (m.bits() == 0, m.bits() == Mods::SHIFT);
        let submits = match submit {
            SubmitKey::Enter => plain,
            SubmitKey::ModEnter => m.bits() == Mods::COMMAND,
            SubmitKey::None => false,
        };
        return if submits {
            Some(KeyAction::Submit)
        } else if multiline && (plain || shift) {
            Some(KeyAction::Newline)
        } else {
            None
        };
    }
    // Printable text inserts unless a command chord is held.
    if !m.meta
        && !m.ctrl
        && let Some(t) = &k.text
        && !t.is_empty()
    {
        return Some(KeyAction::Insert(t.clone()));
    }
    if m.meta || m.ctrl {
        let command = |c| k.is(Mods::COMMAND, c);
        return Some(match k.key {
            _ if command('a') => KeyAction::SelectAll,
            _ if command('c') => KeyAction::Copy,
            _ if command('x') => KeyAction::Cut,
            _ if command('v') => KeyAction::Paste,
            _ if command('z') => KeyAction::Undo,
            _ if command('y') || k.is(Mods::COMMAND | Mods::SHIFT, 'z') => KeyAction::Redo,
            Key::Left if m.shift => KeyAction::SelectTextStart,
            Key::Left => KeyAction::MoveTextStart,
            Key::Right if m.shift => KeyAction::SelectTextEnd,
            Key::Right => KeyAction::MoveTextEnd,
            Key::Up if m.shift => KeyAction::SelectTextStart,
            Key::Up => KeyAction::MoveTextStart,
            Key::Down if m.shift => KeyAction::SelectTextEnd,
            Key::Down => KeyAction::MoveTextEnd,
            _ => return None,
        });
    }
    let sel = m.shift;
    Some(match k.key {
        Key::Backspace if m.alt => KeyAction::BackspaceWord,
        Key::Backspace => KeyAction::Backspace,
        Key::Delete if m.alt => KeyAction::DeleteWord,
        Key::Delete => KeyAction::Delete,
        Key::Tab => return None, // traversal handled above
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
        _ => return None,
    })
}
