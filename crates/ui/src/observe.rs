//! What JS observes of geometry and presentation (ARCHITECTURE-update
//! topic 14): layout events for nodes that listen for them, answers to
//! `Measure`, the window's state, and answers to `Present` once a frame
//! is on screen.
//!
//! Layout events are change-driven: a layout pass records the nodes
//! whose box moved or resized (`Layouts::moved`, `resized`), and only
//! those with a listener are compared with what JS last heard. A new
//! listener reports once, at the first layout that has placed its node.

use std::collections::HashMap;

use craie_core::{Rect, Size};

use crate::events::{UiEvent, out_kind, window_bit};
use crate::host::NodeId;
use crate::ui::Ui;

/// The window as JS sees it (`out_kind::WINDOW`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowState {
    /// Logical points.
    pub size: Size,
    /// Physical pixels per logical point.
    pub scale: f32,
    pub focused: bool,
    /// Not minimized and not fully covered.
    pub visible: bool,
    /// The system appearance is dark.
    pub dark: bool,
}

/// A `Present` command waiting for its frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PresentRequest {
    pub request: u32,
    /// Waits for a frame at rest (`Ui::at_rest`).
    pub rest: bool,
    /// Where to write the frame as a PNG.
    pub path: Option<String>,
}

#[derive(Default)]
pub(crate) struct Observe {
    /// Nodes with a layout listener, by id, and the box JS last heard
    /// (`None`: nothing yet). A slot's new occupant starts without one
    /// (`Ui::forget_node_state`).
    layout: HashMap<u32, Option<Rect>>,
    /// Listeners not reported yet: they report at the first layout of
    /// their node, whether or not it moved.
    fresh: Vec<u32>,
    /// `Measure` commands, with the node's generation then.
    measures: Vec<(NodeId, u16, u32)>,
    presents: Vec<PresentRequest>,
    /// The window state JS last heard.
    window: Option<WindowState>,
    /// Frames presented so far.
    frames: u32,
}

impl Observe {
    /// A node's listener mask changed from `before` to `after`.
    pub(crate) fn listeners(&mut self, id: u32, before: u32, after: u32) {
        let bit = crate::events::mask::LAYOUT;
        if after & bit != 0 && before & bit == 0 {
            self.layout.insert(id, None);
            self.fresh.push(id);
        } else if after & bit == 0 && before & bit != 0 {
            self.layout.remove(&id);
        }
    }

    pub(crate) fn forget(&mut self, id: u32) {
        self.layout.remove(&id);
    }

    /// Answers are owed before the UI can idle: a frame must come.
    pub(crate) fn owed(&self) -> bool {
        !self.presents.is_empty()
    }
}

impl Ui {
    /// Sends what layout observers have not heard: the nodes whose box
    /// changed in the passes since the last paint, and new listeners
    /// whose node is laid out. Runs after layout, before the paint that
    /// drains `moved` and `resized`.
    pub(crate) fn report_layouts(&mut self) {
        if self.observe.layout.is_empty() {
            self.observe.fresh.clear();
            return;
        }
        let changed = self.layouts.moved.len() + self.layouts.resized.len();
        for i in 0..changed {
            let id = match self.layouts.moved.as_slice().get(i) {
                Some(&id) => id,
                None => self.layouts.resized.as_slice()[i - self.layouts.moved.len()],
            };
            self.report_layout(id);
        }
        let mut fresh = std::mem::take(&mut self.observe.fresh);
        // Not laid out yet (detached, or created after the pass): later.
        fresh.retain(|&id| {
            if !self.observe.layout.contains_key(&id) {
                return false;
            }
            if !self.layouts.is_laid_out(NodeId(id)) {
                return true;
            }
            self.report_layout(id);
            false
        });
        self.observe.fresh = fresh;
    }

    fn report_layout(&mut self, id: u32) {
        let node = NodeId(id);
        let Some(sent) = self.observe.layout.get(&id) else {
            return;
        };
        if !self.host.is_live(node) || !self.layouts.is_laid_out(node) {
            return;
        }
        let rect = self.layouts.data(node).rect;
        if *sent == Some(rect) {
            return;
        }
        self.observe.layout.insert(id, Some(rect));
        let mut e = self.event(out_kind::LAYOUT, node);
        e.x = rect.origin.x;
        e.y = rect.origin.y;
        e.a = rect.size.width;
        e.b = rect.size.height;
        self.pending_events.push(e);
    }

    /// Queues a `Measure` answer for the next layout.
    pub(crate) fn measure(&mut self, node: NodeId, request: u32) {
        let generation = self.host.node(node).map_or(0, |n| n.generation);
        self.observe.measures.push((node, generation, request));
    }

    /// Answers the queued measures from current geometry.
    pub(crate) fn answer_measures(&mut self) {
        for (node, generation, request) in std::mem::take(&mut self.observe.measures) {
            let mut e = UiEvent::new(out_kind::MEASURE, node.0);
            e.generation = generation;
            e.key = request;
            let same = self
                .host
                .node(node)
                .is_some_and(|n| n.generation == generation);
            if same && self.layouts.is_laid_out(node) && self.displayed(node) {
                let r = self.abs_rect(node);
                e.revision = 1;
                e.x = r.origin.x;
                e.y = r.origin.y;
                e.a = r.size.width;
                e.b = r.size.height;
            }
            self.pending_events.push(e);
        }
    }

    /// In the tree, and neither it nor an ancestor is `display: none`.
    fn displayed(&self, id: NodeId) -> bool {
        let mut cur = id;
        loop {
            if !self.host.is_live(cur) || self.host.display_none(cur) {
                return false;
            }
            let p = self.host.parent(cur);
            if p.is_nil() {
                return true;
            }
            if !p.is_node() {
                return false;
            }
            cur = p;
        }
    }

    /// Answers what waits for layout when no frame is owed: layout is
    /// current, so nothing else would answer them.
    pub(crate) fn answer_if_current(&mut self) {
        if !self.needs_paint() {
            self.report_layouts();
            self.answer_measures();
        }
    }

    /// Sets the window's state; JS hears of a change (`WINDOW`). The
    /// size also sets the breakpoints (`set_window_size`).
    pub fn set_window(&mut self, w: WindowState) {
        self.set_window_size(w.size);
        if self.observe.window == Some(w) {
            return;
        }
        self.observe.window = Some(w);
        let mut e = UiEvent::new(out_kind::WINDOW, crate::mutation::NIL);
        e.x = w.size.width;
        e.y = w.size.height;
        e.a = w.scale;
        e.key = if w.focused { window_bit::FOCUSED } else { 0 }
            | if w.visible { window_bit::VISIBLE } else { 0 }
            | if w.dark { window_bit::DARK } else { 0 };
        self.pending_events.push(e);
    }

    pub(crate) fn request_present(&mut self, request: PresentRequest) {
        self.observe.presents.push(request);
    }

    /// Nothing moves or waits: no paint owed, no animation, no moving
    /// space, no image decode in flight. A frame drawn now stays.
    pub fn at_rest(&self) -> bool {
        !self.animating()
            && !self.needs_paint_ignoring_presents()
            && self.next_settle().is_none()
            && !self.images.busy()
    }

    /// The platform presented a frame of the current scene: returns the
    /// frame's number and the `Present` requests it answers (all, or
    /// all but those waiting for rest), for the platform to capture
    /// and then `presented`.
    pub fn frame_presented(&mut self) -> (u32, Vec<PresentRequest>) {
        self.observe.frames = self.observe.frames.wrapping_add(1);
        let rest = self.at_rest();
        let (due, wait) = std::mem::take(&mut self.observe.presents)
            .into_iter()
            .partition(|p| rest || !p.rest);
        self.observe.presents = wait;
        (self.observe.frames, due)
    }

    /// Answers a `Present` request with the frame that showed it, its
    /// size in pixels, and why its capture failed, if it did.
    pub fn presented(&mut self, request: u32, frame: u32, size: (u32, u32), error: Option<String>) {
        let mut e = UiEvent::new(out_kind::PRESENTED, crate::mutation::NIL);
        e.key = request;
        e.revision = frame;
        e.x = size.0 as f32;
        e.y = size.1 as f32;
        e.text = error.unwrap_or_default();
        self.pending_events.push(e);
    }
}
