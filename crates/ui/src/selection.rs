//! Cross-node read-only text selection (ARCHITECTURE.md §5, step 3c).
//!
//! A `selectable` node makes its text descendants (itself included) one
//! selection domain. A selection has an anchor and a focus, each a byte
//! offset in one of the domain's text nodes; between them, in tree
//! order, every text node is selected whole. Copy yields the selected
//! text in tree order, one line per paragraph. The highlight is drawn in
//! each text node's chunk from its paragraph's placements.

use std::ops::Range;

use craie_core::geom::Point;

use crate::host::NodeId;
use crate::mutation::NodeKind;
use crate::ui::Ui;

/// A byte offset in a text node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPoint {
    pub node: NodeId,
    pub offset: u32,
}

/// A selection in one domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    pub domain: NodeId,
    pub anchor: TextPoint,
    pub focus: TextPoint,
}

/// An interval end: (index among the domain's texts, byte offset).
type Endpoint = (usize, u32);

/// The highlight fill (as inputs'): rgba(53,132,228,0.48).
pub const SELECTION_COLOR: u32 = 0x3584_E47A;

impl Ui {
    /// The nearest selectable node on `id`'s path (itself included).
    pub(crate) fn selection_domain(&self, id: NodeId) -> Option<NodeId> {
        self.path_to(id)
            .into_iter()
            .find(|&n| self.host.interaction(n).selectable)
    }

    /// The domain's text nodes in tree order (displayed ones only).
    pub(crate) fn domain_texts(&self, domain: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![domain];
        while let Some(id) = stack.pop() {
            if self.host.node(id).is_none() || self.host.style(id).display == taffy::Display::None {
                continue;
            }
            if self.host.kind(id) == Some(NodeKind::Text) {
                out.push(id);
            }
            stack.extend(self.host.children(id).iter().rev().copied());
        }
        out
    }

    /// The text position for window point (x, y) in `domain`: in `hit`
    /// when it is one of the domain's texts, else in the text whose
    /// content box is nearest the point (both axes, in window space;
    /// the first in tree order on a tie).
    pub(crate) fn text_position(
        &self,
        domain: NodeId,
        hit: Option<NodeId>,
        x: f32,
        y: f32,
    ) -> Option<TextPoint> {
        let texts = self.domain_texts(domain);
        let target = match hit.filter(|h| texts.contains(h)) {
            Some(h) => h,
            None => {
                let mut best: Option<(f32, NodeId)> = None;
                for &t in &texts {
                    let d = self.window_distance(t, x, y);
                    if best.is_none_or(|(b, _)| d < b) {
                        best = Some((d, t));
                    }
                }
                best?.1
            }
        };
        let (lx, ly) = self.to_content(target, x, y);
        let offset = self.text_layout(target).map_or(0, |p| p.hit(lx, ly).offset);
        Some(TextPoint {
            node: target,
            offset,
        })
    }

    /// Distance in window space from (x, y) to text `t`'s content box
    /// as drawn: 0 inside it, else to the nearest edge of the
    /// transformed box (a parallelogram), so shear and non-uniform scale
    /// measure true distances.
    pub(crate) fn window_distance(&self, t: NodeId, x: f32, y: f32) -> f32 {
        let data = self.layouts.data(t);
        let w = (data.rect.size.width - data.insets[0]).max(0.0);
        let h = (data.rect.size.height - data.insets[1]).max(0.0);
        let (lx, ly) = self.to_content(t, x, y);
        if (0.0..=w).contains(&lx) && (0.0..=h).contains(&ly) {
            return 0.0;
        }
        let m = self.node_to_window(t);
        let [cx, cy] = data.content;
        let corner = |u: f32, v: f32| m.apply(Point::new(cx + u, cy + v));
        let q = [
            corner(0.0, 0.0),
            corner(w, 0.0),
            corner(w, h),
            corner(0.0, h),
        ];
        let p = Point::new(x, y);
        (0..4)
            .map(|k| segment_distance(p, q[k], q[(k + 1) % 4]))
            .fold(f32::INFINITY, f32::min)
    }

    /// The highlighted range of text node `id`, clamped onto its text.
    pub(crate) fn highlight_of(&self, id: NodeId) -> Option<Range<u32>> {
        let r = self
            .selection_highlight
            .iter()
            .find(|(n, _)| *n == id)?
            .1
            .clone();
        let text = &self.host.paragraph(id)?.text;
        let clamp = |i: u32| {
            let mut i = (i as usize).min(text.len());
            while !text.is_char_boundary(i) {
                i -= 1;
            }
            i as u32
        };
        let r = clamp(r.start)..clamp(r.end);
        (!r.is_empty()).then_some(r)
    }

    /// The selection as a tree interval: the domain's texts, and the
    /// (index, offset) of its start and end. Offsets past a paragraph's
    /// text (it changed) are clamped onto character boundaries.
    fn selection_interval(&self) -> Option<(Vec<NodeId>, Endpoint, Endpoint)> {
        let sel = self.text_selection?;
        let texts = self.domain_texts(sel.domain);
        let index = |p: &TextPoint| texts.iter().position(|&t| t == p.node);
        let (ia, ifo) = (index(&sel.anchor)?, index(&sel.focus)?);
        let clamp = |p: &TextPoint| {
            let text = self.host.paragraph(p.node).map_or("", |q| q.text.as_str());
            let mut i = (p.offset as usize).min(text.len());
            while !text.is_char_boundary(i) {
                i -= 1;
            }
            i as u32
        };
        let (a, f) = ((ia, clamp(&sel.anchor)), (ifo, clamp(&sel.focus)));
        let (start, end) = if a <= f { (a, f) } else { (f, a) };
        Some((texts, start, end))
    }

    /// Each text node of the interval with its selected range (possibly
    /// empty), in tree order.
    fn selection_pieces(&self) -> Vec<(NodeId, Range<u32>)> {
        let Some((texts, start, end)) = self.selection_interval() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (k, &t) in texts.iter().enumerate().take(end.0 + 1).skip(start.0) {
            let len = self.host.paragraph(t).map_or(0, |p| p.text.len() as u32);
            let from = if k == start.0 { start.1 } else { 0 };
            let to = if k == end.0 { end.1 } else { len };
            out.push((t, from..to.max(from)));
        }
        out
    }

    /// The selected range of each text node (non-empty ones), in tree
    /// order: what highlights draw.
    pub fn selection_ranges(&self) -> Vec<(NodeId, Range<u32>)> {
        let mut out = self.selection_pieces();
        out.retain(|(_, r)| !r.is_empty());
        out
    }

    /// The selected text in tree order, one line per paragraph of the
    /// interval (an empty paragraph is an empty line).
    pub fn selected_text(&self) -> String {
        let mut out = String::new();
        for (k, (t, r)) in self.selection_pieces().into_iter().enumerate() {
            if k > 0 {
                out.push('\n');
            }
            if let Some(p) = self.host.paragraph(t) {
                out.push_str(&p.text[r.start as usize..r.end as usize]);
            }
        }
        out
    }

    /// The current selection.
    pub fn text_selection(&self) -> Option<TextSelection> {
        self.text_selection
    }

    /// Replaces the selection; text nodes whose highlight changed rebuild
    /// their chunk. A selection whose nodes are not live, whose domain
    /// is not selectable, or whose endpoints are not displayed texts of
    /// the domain is dropped.
    pub fn set_text_selection(&mut self, next: Option<TextSelection>) {
        self.text_selection = next;
        self.selection_generations = next.map_or([0; 3], |s| {
            [s.domain, s.anchor.node, s.focus.node]
                .map(|n| self.host.node(n).map_or(0, |h| h.generation))
        });
        self.refresh_selection();
    }

    /// Revalidates the selection against the tree and redraws the text
    /// nodes whose highlighted range changed. Runs after every
    /// transaction and, when the host changed since, before paint (list
    /// rows appear and disappear natively).
    pub(crate) fn refresh_selection(&mut self) {
        if let Some(sel) = self.text_selection
            && !self.selection_valid(sel)
        {
            self.text_selection = None;
            self.selecting = false;
        }
        self.selection_revs = self.host.revs;
        if self.text_selection.is_none() && self.selection_highlight.is_empty() {
            return;
        }
        let after = self.selection_ranges();
        if after == self.selection_highlight {
            return;
        }
        let before = std::mem::replace(&mut self.selection_highlight, after);
        let after = &self.selection_highlight;
        let range_of = |list: &[(NodeId, Range<u32>)], id: NodeId| {
            list.iter().find(|(n, _)| *n == id).map(|(_, r)| r.clone())
        };
        let mut changed: Vec<NodeId> = Vec::new();
        for (id, _) in before.iter().chain(after.iter()) {
            if !changed.contains(id) && range_of(&before, *id) != range_of(after, *id) {
                changed.push(*id);
            }
        }
        for id in changed {
            if self.host.is_live(id) {
                self.host.dirty.content.push(id.0);
            }
        }
        self.force_paint = true;
    }

    /// The selection's nodes are the ones it was made on (same
    /// generation), its domain is selectable and shown (attached, no
    /// hidden ancestor), and both endpoints are displayed texts of the
    /// domain.
    fn selection_valid(&self, sel: TextSelection) -> bool {
        let nodes = [sel.domain, sel.anchor.node, sel.focus.node];
        let same = nodes
            .iter()
            .zip(self.selection_generations)
            .all(|(&n, g)| self.host.node(n).is_some_and(|h| h.generation == g));
        if !same || !self.host.interaction(sel.domain).selectable {
            return false;
        }
        // The domain reaches the root level through displayed ancestors
        // (not detached, not under `display: none`).
        let mut cur = sel.domain;
        loop {
            if self.host.display_none(cur) {
                return false;
            }
            let parent = self.host.parent(cur);
            if parent == NodeId::DETACHED {
                return false;
            }
            if !parent.is_node() {
                break;
            }
            cur = parent;
        }
        let texts = self.domain_texts(sel.domain);
        texts.contains(&sel.anchor.node) && texts.contains(&sel.focus.node)
    }

    /// A primary press at (x, y) on `hit`: starts (or with shift,
    /// extends) a selection in its domain, or clears one outside any.
    /// Returns whether a selection drag began.
    pub(crate) fn selection_press(
        &mut self,
        hit: Option<NodeId>,
        x: f32,
        y: f32,
        shift: bool,
    ) -> bool {
        let domain = hit.and_then(|h| self.selection_domain(h));
        let Some(domain) = domain else {
            self.set_text_selection(None);
            return false;
        };
        let Some(at) = self.text_position(domain, hit, x, y) else {
            self.set_text_selection(None);
            return false;
        };
        let anchor = match self.text_selection {
            Some(s) if shift && s.domain == domain => s.anchor,
            _ => at,
        };
        self.set_text_selection(Some(TextSelection {
            domain,
            anchor,
            focus: at,
        }));
        true
    }

    /// A drag to (x, y) moves the focus.
    pub(crate) fn selection_drag(&mut self, x: f32, y: f32) {
        let Some(sel) = self.text_selection else {
            return;
        };
        let hit = self.hit_test(x, y);
        if let Some(focus) = self.text_position(sel.domain, hit, x, y) {
            self.set_text_selection(Some(TextSelection { focus, ..sel }));
        }
    }

    /// Selects every text of the current selection's domain.
    pub(crate) fn select_domain(&mut self) {
        let Some(sel) = self.text_selection else {
            return;
        };
        let texts = self.domain_texts(sel.domain);
        let (Some(&first), Some(&last)) = (texts.first(), texts.last()) else {
            return;
        };
        let len = self.host.paragraph(last).map_or(0, |p| p.text.len() as u32);
        self.set_text_selection(Some(TextSelection {
            domain: sel.domain,
            anchor: TextPoint {
                node: first,
                offset: 0,
            },
            focus: TextPoint {
                node: last,
                offset: len,
            },
        }));
    }
}

/// Distance from `p` to the segment `a`..`b`.
fn segment_distance(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.x - (a.x + t * dx)).hypot(p.y - (a.y + t * dy))
}
