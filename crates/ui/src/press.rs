//! Presses and activation (ARCHITECTURE-update topic 3).
//!
//! Nodes marked pressable (`mutation::press`) own presses: a primary
//! press goes to the innermost pressable on the hit path, and only to
//! it (`PRESS` in, out or cancel), while raw pointer listeners keep the
//! whole path. A disabled pressable swallows its presses. Activation is
//! one `ACTIVATE` event to the pressable itself, with no hit test, from
//! each source:
//!
//! - a primary press released over the node it started on, or over
//!   anything inside it (React Aria's rule; the web clicks the nearest
//!   common ancestor of the press and the release, LEDGER.md DF-45);
//! - Enter on key down, or Space on key up, on a focused pressable, with
//!   any modifiers (a key claim on the chord wins);
//! - an accessibility click on the pressable or inside it.
//!
//! Focus groups activate the member they reach through `activate` too.

use craie_core::geom::Point;

use crate::events::{Key, KeyInput, Mods, UiEvent, activate_source, mask, out_kind, press_phase};
use crate::host::NodeId;
use crate::mutation::press;
use crate::ui::Ui;

/// A primary press in progress on a pressable.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Press {
    pub(crate) node: NodeId,
    generation: u16,
    /// The span under the press's start, plus one (0: none), on a text
    /// node: a pressable span activates only when released on its own
    /// pressable (the run of spans it joins).
    span: u32,
    /// The paragraph's revision at the press (0: none): span indices
    /// mean nothing across span tables.
    revision: u32,
    /// The modifiers at the press's start: its activation carries them
    /// (a release reports none).
    mods: Mods,
}

impl Ui {
    /// Whether `id` is a pressable (node-level, not a span's) and not
    /// disabled: what keys and assistive technology activate.
    pub(crate) fn enabled_pressable(&self, id: NodeId) -> bool {
        self.host.interaction(id).press & (press::PRESSABLE | press::DISABLED) == press::PRESSABLE
    }

    /// Whether span `span` (plus one; 0: none) of text node `id` is
    /// pressable.
    fn span_pressable(&self, id: NodeId, span: u32) -> bool {
        span != 0
            && self
                .host
                .paragraph(id)
                .and_then(|p| p.spans.get(span as usize - 1))
                .is_some_and(|s| s.pressable)
    }

    /// The pressable run span `span` (plus one) of text node `id` is in:
    /// its first span, plus one (0: not a pressable span).
    fn press_run(&self, id: NodeId, span: u32) -> u32 {
        let Some(spans) = self.host.paragraph(id).map(|p| &p.spans) else {
            return 0;
        };
        let mut i = span as usize;
        if i == 0 || !spans.get(i - 1).is_some_and(|s| s.pressable) {
            return 0;
        }
        while i > 1 && spans[i - 1].press_joins && spans[i - 2].pressable {
            i -= 1;
        }
        i as u32
    }

    fn revision(&self, id: NodeId) -> u32 {
        self.host.paragraph(id).map_or(0, |p| p.revision)
    }

    /// The innermost pressable on `hit`'s path at window point (x, y),
    /// disabled ones included (they swallow the press), with the span
    /// under the point plus one (0: none) when it is a text node.
    fn pressable_at(&self, hit: NodeId, x: f32, y: f32) -> Option<(NodeId, u32)> {
        self.ancestors(hit).find_map(|n| {
            let span = self.span_at(n, x, y).map_or(0, |s| s + 1);
            let own = self.host.interaction(n).press & press::PRESSABLE != 0;
            (own || self.span_pressable(n, span)).then_some((n, span))
        })
    }

    fn press_live(&self, p: &Press) -> bool {
        self.host
            .node(p.node)
            .is_some_and(|n| n.generation == p.generation)
    }

    /// A primary button went down on `hit`: a press of its innermost
    /// pressable begins (`PRESS` in), unless that one is disabled. A
    /// press still held (its release was lost) is cancelled first.
    pub(crate) fn press_down(&mut self, hit: Option<NodeId>, x: f32, y: f32, mods: Mods) {
        self.cancel_press();
        let Some((node, span)) = hit.and_then(|h| self.pressable_at(h, x, y)) else {
            return;
        };
        if self.host.interaction(node).press & press::DISABLED != 0 {
            return;
        }
        let generation = self.host.node(node).map_or(0, |n| n.generation);
        let revision = self.revision(node);
        self.press = Some(Press {
            node,
            generation,
            span,
            revision,
            mods,
        });
        let phase = press_phase::IN;
        self.emit_press(
            node,
            out_kind::PRESS,
            phase,
            1,
            (x, y),
            (span, revision),
            mods,
        );
    }

    /// The primary button came up at (x, y): the press ends (`PRESS`
    /// out) and, when the release is over the pressed node or inside
    /// it, activates it. A pressed span activates when released on its
    /// own pressable; elsewhere on its node only if the node is
    /// pressable.
    pub(crate) fn press_up(&mut self, x: f32, y: f32) {
        let Some(p) = self.press.take() else {
            return;
        };
        if !self.press_live(&p) {
            return;
        }
        let flags = self.host.interaction(p.node).press;
        let own = flags & press::PRESSABLE != 0;
        let pressed = (p.span, p.revision);
        // A new span table: the pressed span may be another Text's now.
        let retabled = self.revision(p.node) != p.revision;
        let span_press = !retabled && self.span_pressable(p.node, p.span);
        if flags & press::DISABLED != 0 || !(own || span_press) {
            // Disabled, unmarked or retabled since it began. The cancel
            // carries the old revision, like the press it ends.
            let phase = press_phase::CANCEL;
            self.emit_press(p.node, out_kind::PRESS, phase, 1, (x, y), pressed, p.mods);
            return;
        }
        let phase = press_phase::OUT;
        self.emit_press(p.node, out_kind::PRESS, phase, 1, (x, y), pressed, p.mods);
        let hit = self.hit_test(x, y);
        if !hit.is_some_and(|h| self.ancestors(h).any(|n| n == p.node)) {
            return;
        }
        let at = if hit == Some(p.node) {
            self.span_at(p.node, x, y).map_or(0, |s| s + 1)
        } else {
            0
        };
        // Released on the pressed span or another of its pressable's
        // (its run): it activates. Elsewhere, a pressable node activates
        // as a whole, a pressable span not at all.
        let run = self.press_run(p.node, p.span);
        let same = !retabled && (at == p.span || (run != 0 && self.press_run(p.node, at) == run));
        let span = match (same, own) {
            (true, _) => p.span,
            (false, true) => 0,
            (false, false) => return,
        };
        let (source, at) = (activate_source::POINTER, (span, p.revision));
        self.emit_press(p.node, out_kind::ACTIVATE, source, 1, (x, y), at, p.mods);
    }

    /// Ends the press without a release (window focus lost, the node
    /// leaving the tree): `PRESS` cancel.
    pub(crate) fn cancel_press(&mut self) {
        let Some(p) = self.press.take() else {
            return;
        };
        if self.press_live(&p) {
            let at = self.last_pointer.unwrap_or_else(|| self.center(p.node));
            let (phase, pressed) = (press_phase::CANCEL, (p.span, p.revision));
            self.emit_press(p.node, out_kind::PRESS, phase, 1, at, pressed, p.mods);
        }
    }

    /// Activates pressable `id` from `source` (`activate_source`): one
    /// `ACTIVATE` to it, at its center, with no hit test. Nothing
    /// happens, and false returns, when `id` is not an enabled pressable.
    pub(crate) fn activate(&mut self, id: NodeId, source: u32, mods: Mods) -> bool {
        if !self.enabled_pressable(id) {
            return false;
        }
        let at = self.center(id);
        self.emit_press(id, out_kind::ACTIVATE, source, 0, at, (0, 0), mods);
        true
    }

    /// A key down on the focused node, after claims: Enter activates a
    /// focused pressable (each repeat too, as browsers do), and Space
    /// begins a key press that its key up activates. Modifiers stop
    /// neither, as in browsers (Cmd+Enter on a link opens a new tab):
    /// the activation carries them, and a claim on the chord wins.
    pub(crate) fn press_key_down(&mut self, k: &KeyInput) {
        let Some(f) = self.focus else {
            return;
        };
        if !self.enabled_pressable(f) {
            return;
        }
        match k.key {
            Key::Enter => {
                self.activate(f, activate_source::KEY, k.mods);
            }
            Key::Space if !k.repeat => self.key_press = Some(f),
            _ => {}
        }
    }

    /// A key up: Space ends the key press it began and activates the
    /// node, if it still has focus, with the modifiers held now.
    pub(crate) fn press_key_up(&mut self, k: &KeyInput) {
        if k.key == Key::Space
            && let Some(n) = self.key_press.take()
            && self.focus == Some(n)
        {
            self.activate(n, activate_source::KEY, k.mods);
        }
    }

    /// The node's border-box center, in window points.
    fn center(&self, id: NodeId) -> (f32, f32) {
        let size = self.layouts.data(id).rect.size;
        let c = self
            .node_to_window(id)
            .apply(Point::new(size.width / 2.0, size.height / 2.0));
        (c.x, c.y)
    }

    /// Queues a `PRESS` or `ACTIVATE` for `id` when it listens: `bits`
    /// are the phase or source (key bits 4 and 5), `button` 1 for the
    /// primary and 0 for none, and the span (plus one) comes with the
    /// paragraph revision it indexes.
    #[allow(clippy::too_many_arguments)]
    fn emit_press(
        &mut self,
        id: NodeId,
        kind: u8,
        bits: u32,
        button: u32,
        (x, y): (f32, f32),
        (span, revision): (u32, u32),
        mods: Mods,
    ) {
        let want = if kind == out_kind::PRESS {
            mask::PRESS
        } else {
            mask::ACTIVATE
        };
        if self.host.interaction(id).listeners & want == 0 {
            return;
        }
        let local = self
            .node_to_window(id)
            .invert()
            .map_or(Point::new(0.0, 0.0), |m| m.apply(Point::new(x, y)));
        let mut e: UiEvent = self.event(kind, id);
        e.x = x;
        e.y = y;
        e.a = local.x;
        e.b = local.y;
        e.key = mods.bits() as u32 | bits << 4 | button << 8 | span << 16;
        if span != 0 {
            e.revision = revision;
        }
        self.pending_events.push(e);
    }
}
