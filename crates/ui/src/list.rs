//! Virtualized lists (ARCHITECTURE.md §7).
//!
//! A `List` node lives inside a scroll container. It owns its item
//! count, one extent per item (a native estimate until the item's row
//! renders and is measured), and the range it last reported. React
//! renders only the rows in that range; each rendered row is a child of
//! the list tagged with its item index (`ListIndex`). Layout visits only
//! rendered rows and places them at their item offsets.
//!
//! Estimates come from a simplified item description in the list ops (a
//! row template and a text length): native knows the font metrics and
//! the width, so no extents copy lives in JS.
//!
//! The scroll container owns the offset and the anchor policy. With
//! `KeepVisible` (the default) the top visible item keeps its place when
//! extents above it change; with `StickToEnd` a scroller at its end
//! stays at its end.

use std::collections::HashMap;
use std::ops::Range;

use craie_core::extents::Extents;

use crate::events::out_kind;
use crate::geom::{Point, Rect, Size};
use crate::host::NodeId;
use crate::mutation::{Anchor, ItemDesc, ItemTemplate, NIL};
use crate::scene::PaintSlot;
use crate::text::parley::style::StyleProperty;
use crate::text::{ParagraphSpec, TextEngine};
use craie_core::Affine;

/// Upper bound on a list's item count: bounds the per-item stores
/// (about 19 bytes per item), and keeps item indices exact in the f32
/// fields of range events.
pub const MAX_ITEMS: u32 = 1 << 24;

/// Sample text for per-size metrics: a mix of letters, digits, and
/// spaces close to running prose.
const SAMPLE: &str = "The quick brown fox jumps over the lazy dog, 0123456789 times.";

pub struct ListState {
    pub templates: Vec<ItemTemplate>,
    pub descs: Vec<ItemDesc>,
    pub extents: Extents,
    /// Extra distance rendered beyond the viewport, each side (logical
    /// points).
    pub overscan: f32,
    /// Estimate of an item whose template is unknown.
    pub fallback: f32,
    /// Content width the estimates and measurements were made at (NaN
    /// before the first layout).
    pub width: f32,
    /// Templates or the fallback changed: estimates are stale.
    pub stale: bool,
    /// Resolved gap between rows (logical points), from the last layout.
    pub gap: f32,
    /// Splices applied: range events carry it so JS can tell which item
    /// order an event's indices refer to.
    pub revision: u32,
    /// Last reported item range, and the item kept rendered for focus
    /// (index and identity).
    pub reported: Range<u32>,
    pub keep: u32,
    pub keep_id: u32,
    /// Items changed since the last report: report again.
    pub resend: bool,
}

impl Default for ListState {
    fn default() -> ListState {
        ListState {
            templates: Vec::new(),
            descs: Vec::new(),
            extents: Extents::new(),
            overscan: 0.0,
            fallback: 0.0,
            width: f32::NAN,
            stale: false,
            gap: 0.0,
            revision: 0,
            reported: 0..0,
            keep: NIL,
            keep_id: NIL,
            resend: false,
        }
    }
}

impl ListState {
    pub fn len(&self) -> u32 {
        self.descs.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.descs.is_empty()
    }

    /// Offset of item `i` in the list's content, gaps included.
    pub fn offset(&self, i: usize) -> f32 {
        self.extents.offset_gap(i, self.gap)
    }

    /// Content height: every item, with gaps between them.
    pub fn total(&self) -> f32 {
        self.extents.total_gap(self.gap)
    }

    /// The item at content offset `y` (a gap belongs to the item above).
    pub fn item_at(&self, y: f32) -> usize {
        self.extents.index_at_gap(y, self.gap)
    }
}

/// A scroller's captured anchor: list item `index` of `list` sat
/// `delta` points below the viewport top (`list` NIL: no list item was
/// visible). `at_end`: the scroller was at its end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Saved {
    pub list: u32,
    pub index: u32,
    pub delta: f32,
    pub at_end: bool,
}

/// Every list's state, the scroll anchors, and per-size text metrics.
/// Lists are few: a map by node id.
#[derive(Default)]
pub struct Lists {
    pub(crate) map: HashMap<u32, ListState>,
    /// Anchor policies that are not the default, by scroller.
    pub(crate) policies: HashMap<u32, Anchor>,
    /// Captured anchors by scroller, refreshed after every frame.
    pub(crate) saved: HashMap<u32, Saved>,
    /// (average advance, line height) per font size (f32 bits).
    metrics: HashMap<u32, (f32, f32)>,
    /// Per-frame scratch: list ids in order, scrollers anchored.
    ids: Vec<u32>,
    anchored: Vec<u32>,
}

impl Lists {
    pub fn get(&self, id: u32) -> Option<&ListState> {
        self.map.get(&id)
    }

    pub fn policy(&self, scroller: u32) -> Anchor {
        self.policies.get(&scroller).copied().unwrap_or_default()
    }

    /// Forgets everything a freed node held.
    pub(crate) fn forget(&mut self, id: u32) {
        self.map.remove(&id);
        self.policies.remove(&id);
        self.saved.remove(&id);
        self.saved.retain(|_, s| s.list != id);
    }

    pub(crate) fn configure(
        &mut self,
        id: u32,
        overscan: f32,
        fallback: f32,
        templates: &[ItemTemplate],
    ) {
        let l = self.map.entry(id).or_default();
        l.overscan = overscan;
        // Unmeasured items estimated from the fallback or the templates
        // are stale when either changes; measurements stay.
        if l.fallback != fallback {
            l.fallback = fallback;
            l.stale = true;
        }
        if l.templates != templates {
            l.templates = templates.to_vec();
            l.stale = true;
        }
    }

    /// Replaces items `at..at + remove` with `items`. An item that moves
    /// within the splice unchanged (same id and description removed and
    /// inserted) keeps its extent and measurement; other new items are estimated at the list's width
    /// when it is known (else at the next layout). A captured anchor
    /// follows its item: to its new place when it moved, to the splice
    /// start only when it was removed.
    pub(crate) fn splice(
        &mut self,
        text: &mut TextEngine,
        id: u32,
        at: u32,
        remove: u32,
        items: &[u8],
    ) {
        let l = self.map.entry(id).or_default();
        let n = l.descs.len() as u32;
        // Validation guarantees the range; clamp rather than panic.
        debug_assert!(at <= n && remove <= n - at.min(n), "splice out of range");
        let at = at.min(n);
        let remove = remove.min(n - at);
        let range = at as usize..(at + remove) as usize;
        let (width, fallback) = (l.width, l.fallback);
        let templates = l.templates.clone();
        // Removed items by identity: (old index, description, extent,
        // measured).
        let removed: HashMap<u32, (u32, ItemDesc, f32, bool)> = range
            .clone()
            .filter(|&i| l.descs[i].id != NIL)
            .map(|i| {
                let d = l.descs[i];
                (
                    d.id,
                    (i as u32, d, l.extents.size(i), l.extents.is_measured(i)),
                )
            })
            .collect();
        let mut new_items = Vec::with_capacity(items.len() / ItemDesc::BYTES);
        // Old index -> new index of items that moved within the splice.
        let mut moved: HashMap<u32, u32> = HashMap::new();
        for (k, d) in ItemDesc::iter(items).enumerate() {
            if let Some(&(old, before, e, m)) = removed.get(&d.id) {
                moved.insert(old, at + k as u32);
                // Unchanged, it keeps its extent; edited, it is estimated
                // again (its row, if rendered, measures it).
                if before == d {
                    new_items.push((e, m));
                    continue;
                }
            }
            let e = match templates.get(d.template as usize) {
                Some(t) if width.is_finite() => {
                    let m = if t.font_size > 0.0 {
                        self.metrics(text, t.font_size)
                    } else {
                        (1.0, 0.0)
                    };
                    estimate(t, m, d.text_len, width)
                }
                _ => fallback,
            };
            new_items.push((e, false));
        }
        let l = self.map.get_mut(&id).unwrap();
        let inserted = new_items.len();
        l.descs.splice(range.clone(), ItemDesc::iter(items));
        l.extents.splice_items(range, new_items);
        if !width.is_finite() {
            l.stale = true;
        }
        l.revision = l.revision.wrapping_add(1);
        l.resend = true;
        let shift = inserted as i64 - remove as i64;
        for s in self.saved.values_mut().filter(|s| s.list == id) {
            if s.index >= at + remove {
                s.index = (s.index as i64 + shift) as u32;
            } else if s.index >= at {
                s.index = match moved.get(&s.index) {
                    Some(&to) => to,
                    // The anchor item went away: anchor to its place.
                    None => at,
                };
            }
        }
    }

    /// (average advance, line height) at `font_size`, measured once by
    /// shaping a sample.
    fn metrics(&mut self, text: &mut TextEngine, font_size: f32) -> (f32, f32) {
        *self.metrics.entry(font_size.to_bits()).or_insert_with(|| {
            let layout = text.layout_paragraph(
                &ParagraphSpec {
                    text: SAMPLE,
                    defaults: &[
                        StyleProperty::FontSize(font_size),
                        StyleProperty::Brush(PaintSlot(0)),
                    ],
                    spans: &[],
                },
                None,
            );
            let advance = layout.width() / SAMPLE.chars().count() as f32;
            (advance.max(0.01), layout.height())
        })
    }

    /// Brings list `id`'s estimates up to date for content width
    /// `width`. A width change forgets measurements (they were made at
    /// another width); rendered rows measure again in the same pass.
    pub(crate) fn estimate(&mut self, text: &mut TextEngine, id: u32, width: f32) {
        let Some(l) = self.map.get(&id) else { return };
        let resized = l.width != width;
        if !resized && !l.stale {
            return;
        }
        // Metrics per template, then one pass over the items.
        let sizes: Vec<f32> = l.templates.iter().map(|t| t.font_size).collect();
        let metrics: Vec<(f32, f32)> = sizes
            .iter()
            .map(|&fs| {
                if fs > 0.0 {
                    self.metrics(text, fs)
                } else {
                    (1.0, 0.0)
                }
            })
            .collect();
        let l = self.map.get_mut(&id).unwrap();
        let (templates, descs, fallback) = (&l.templates, &l.descs, l.fallback);
        l.extents.reestimate(resized, |i| {
            let d = descs[i];
            match templates.get(d.template as usize) {
                Some(t) => estimate(t, metrics[d.template as usize], d.text_len, width),
                None => fallback,
            }
        });
        l.width = width;
        l.stale = false;
    }
}

impl Lists {
    /// Measurements of list `id` (index, extent) and the width they were
    /// made at: what a rebuilt list needs to match (test support).
    pub fn measurements(&self, id: u32) -> (f32, Vec<(u32, f32)>) {
        let Some(l) = self.map.get(&id) else {
            return (f32::NAN, Vec::new());
        };
        let m = (0..l.len())
            .filter(|&i| l.extents.is_measured(i as usize))
            .map(|i| (i, l.extents.size(i as usize)))
            .collect();
        (l.width, m)
    }

    /// Restores measurements taken at `width` into list `id` (test
    /// support: rebuilding a list from a snapshot).
    pub fn restore_measurements(&mut self, id: u32, width: f32, m: &[(u32, f32)]) {
        let Some(l) = self.map.get_mut(&id) else {
            return;
        };
        l.width = width;
        l.stale = true;
        for &(i, e) in m {
            if (i as usize) < l.extents.len() {
                l.extents.measure(i as usize, e);
            }
        }
    }

    /// List `id`'s content height at `width` without recording anything:
    /// current extents with `rows` (index, extent) replacing theirs. With
    /// stale estimates or at another width it sums fresh estimates, and
    /// measurements where they were made at `width` (O(n)).
    pub(crate) fn total_at(
        &mut self,
        text: &mut TextEngine,
        id: u32,
        width: f32,
        gap: f32,
        rows: &[(u32, f32)],
    ) -> f32 {
        let Some(l) = self.map.get(&id) else {
            return 0.0;
        };
        let gaps = l.descs.len().saturating_sub(1) as f32 * gap;
        if l.width == width && !l.stale {
            let mut t = l.extents.total();
            for &(i, e) in rows {
                t += e - l.extents.size(i as usize);
            }
            return t + gaps;
        }
        let sizes: Vec<f32> = l.templates.iter().map(|t| t.font_size).collect();
        let metrics: Vec<(f32, f32)> = sizes
            .iter()
            .map(|&fs| {
                if fs > 0.0 {
                    self.metrics(text, fs)
                } else {
                    (1.0, 0.0)
                }
            })
            .collect();
        let l = &self.map[&id];
        // Measurements hold at their own width only.
        let keep = l.width == width;
        let item = |i: usize| {
            if keep && l.extents.is_measured(i) {
                return l.extents.size(i);
            }
            let d = l.descs[i];
            match l.templates.get(d.template as usize) {
                Some(t) => estimate(t, metrics[d.template as usize], d.text_len, width),
                None => l.fallback,
            }
        };
        let mut t: f64 = (0..l.descs.len()).map(|i| item(i) as f64).sum();
        for &(i, e) in rows {
            t += (e - item(i as usize)) as f64;
        }
        t as f32 + gaps
    }
}

/// An item's estimated extent: the template's fixed part plus its text
/// wrapped at the list width minus the template's insets.
pub fn estimate(t: &ItemTemplate, (advance, line): (f32, f32), text_len: u32, width: f32) -> f32 {
    if text_len == 0 || t.font_size <= 0.0 {
        return t.base;
    }
    let avail = (width - t.inset).max(advance);
    let lines = (text_len as f32 * advance / avail).ceil().max(1.0);
    t.base + lines * line
}

/// Where a list sits in its viewport (the nearest vertical scroll
/// container, else the window), through the same transforms paint and
/// hit testing use.
struct Placement {
    scroller: NodeId,
    /// List border box -> scroller border box with the scroller's own
    /// offset not applied (window coordinates for the window). Composes
    /// each node's layout origin and transform, and the offsets of
    /// scroll containers in between.
    to_view: Affine,
    /// The list's content-box origin in its border box.
    content: [f32; 2],
    /// The viewport in the scroller's border box (its padding box).
    viewport: Rect,
    scroll: [f32; 2],
    max_scroll: f32,
    /// Visible part of the list's content, `v0..v1` (may extend past it;
    /// empty when the list maps to nothing).
    v0: f32,
    v1: f32,
}

impl Placement {
    /// Where the top of item offset `y` sits below the viewport top.
    fn view_y(&self, y: f32) -> f32 {
        let p = self
            .to_view
            .apply(Point::new(self.content[0], self.content[1] + y));
        p.y - self.scroll[1] - self.viewport.origin.y
    }
}

/// Distance under which an anchor correction is noise, not motion.
const ANCHOR_EPS: f32 = 1e-3;

/// A node's border box -> its parent's border box (as `scene_sync`).
fn node_local(origin: Point, t: Affine, size: Size) -> Affine {
    Affine::translate(origin.x, origin.y)
        .mul(&t.about(Point::new(size.width / 2.0, size.height / 2.0)))
}

impl crate::host::Host {
    /// Whether list row `row` is laid out and published: its index is in
    /// range, it is displayed, and no earlier displayed row of the list
    /// has the same index. Layout and accessibility share this rule.
    pub fn list_row_shown(&self, list: NodeId, row: NodeId) -> bool {
        let Some(l) = self.lists.get(list.0) else {
            return false;
        };
        let shown = |r: NodeId| {
            self.list_index[r.index()] < l.len() && self.style(r).display != taffy::Display::None
        };
        if !shown(row) {
            return false;
        }
        let index = self.list_index[row.index()];
        for &c in self.children(list) {
            if c == row {
                return true;
            }
            if shown(c) && self.list_index[c.index()] == index {
                return false;
            }
        }
        false
    }
}

impl crate::ui::Ui {
    fn list_placement(&self, list: NodeId, window: Size) -> Option<Placement> {
        self.host.node(list)?;
        let data = self.layouts.data(list);
        let local = |id: NodeId| {
            let d = self.layouts.data(id);
            node_local(
                d.rect.origin,
                self.host.spatial[id.index()].transform,
                d.rect.size,
            )
        };
        let mut to_view = local(list);
        let mut cur = self.host.parent(list);
        let (scroller, viewport, scroll, max_scroll) = loop {
            if !cur.is_node() {
                let view = Rect::new(0.0, 0.0, window.width, window.height);
                break (NodeId::NIL, view, [0.0; 2], 0.0);
            }
            let style = self.host.style(cur);
            let offset = self.host.spatial[cur.index()].scroll;
            if style.overflow.y == taffy::Overflow::Scroll {
                let d = self.layouts.data(cur);
                break (cur, d.clip_box, offset, d.scroll_extent[1]);
            }
            if style.overflow.x == taffy::Overflow::Scroll {
                to_view = Affine::translate(-offset[0], -offset[1]).mul(&to_view);
            }
            to_view = local(cur).mul(&to_view);
            cur = self.host.parent(cur);
        };
        // The visible region, unscrolled, mapped back into the list.
        let seen = Rect::new(
            viewport.origin.x + scroll[0],
            viewport.origin.y + scroll[1],
            viewport.size.width,
            viewport.size.height,
        );
        let (v0, v1) = match to_view.invert() {
            Some(inv) => {
                let r = inv.map_rect(&seen);
                (r.origin.y - data.content[1], r.max_y() - data.content[1])
            }
            None => (f32::INFINITY, f32::NEG_INFINITY),
        };
        Some(Placement {
            scroller,
            to_view,
            content: data.content,
            viewport,
            scroll,
            max_scroll,
            v0,
            v1,
        })
    }

    /// After a layout pass: every scroller with a captured anchor scrolls
    /// so its anchor item keeps its place in the viewport (`KeepVisible`)
    /// or stays at the end (`StickToEnd`, when it was there).
    pub(crate) fn restore_anchors(&mut self) {
        let window = self.laid_out.unwrap_or(Size::ZERO);
        let saved: Vec<(u32, Saved)> = self
            .host
            .lists
            .saved
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect();
        for (scroller, s) in saved {
            let sc = NodeId(scroller);
            if !self.host.is_live(sc) {
                continue;
            }
            let policy = self.host.lists.policy(scroller);
            let target = match policy {
                Anchor::None => continue,
                Anchor::StickToEnd if s.at_end => self.layouts.data(sc).scroll_extent[1],
                _ => {
                    if s.list == NIL {
                        continue;
                    }
                    let Some(p) = self.list_placement(NodeId(s.list), window) else {
                        continue;
                    };
                    let Some(l) = self.host.lists.get(s.list) else {
                        continue;
                    };
                    if p.scroller != sc || s.index >= l.len() {
                        continue;
                    }
                    // The scroll that puts the item `delta` below the top.
                    p.scroll[1] + p.view_y(l.offset(s.index as usize)) - s.delta
                }
            };
            let cur = self.host.spatial[sc.index()].scroll;
            if (target - cur[1]).abs() > ANCHOR_EPS
                && let Some(off) = self.scroll_to(sc, cur[0], target)
            {
                self.scroll_event(sc, off);
            }
        }
    }

    /// After every frame's layout and scroll: captures each scroller's
    /// anchor, and reports each list's range when the viewport left the
    /// rendered range, the items changed, or focus moved to another row.
    pub(crate) fn sync_lists(&mut self, window: Size) {
        if self.host.lists.map.is_empty() {
            return;
        }
        // Lists in a stable order (by id); kept buffers, no allocation.
        let mut ids = std::mem::take(&mut self.host.lists.ids);
        ids.clear();
        ids.extend(self.host.lists.map.keys().copied());
        ids.sort_unstable();
        // Scrollers anchored this frame (the lowest list id wins).
        let mut anchored = std::mem::take(&mut self.host.lists.anchored);
        anchored.clear();
        for &id in &ids {
            let list = NodeId(id);
            let Some(p) = self.list_placement(list, window) else {
                continue;
            };
            let keep = self.focused_row(list);
            let l = &self.host.lists.map[&id];
            let count = l.len();
            let total = l.total();
            let visible = count > 0 && p.v1 > 0.0 && p.v0 < total && p.v0 <= p.v1;

            if p.scroller.is_node() && !anchored.contains(&p.scroller.0) {
                let at_end = p.scroll[1] >= p.max_scroll - 0.5;
                let saved = if visible {
                    let index = l.item_at(p.v0.max(0.0));
                    Saved {
                        list: id,
                        index: index as u32,
                        delta: p.view_y(l.offset(index)),
                        at_end,
                    }
                } else {
                    Saved {
                        list: NIL,
                        index: 0,
                        delta: 0.0,
                        at_end,
                    }
                };
                // A scroller showing no list item keeps no item anchor.
                if visible || !self.host.lists.saved.contains_key(&p.scroller.0) {
                    self.host.lists.saved.insert(p.scroller.0, saved);
                } else if let Some(s) = self.host.lists.saved.get_mut(&p.scroller.0) {
                    s.list = NIL;
                    s.at_end = at_end;
                }
                if visible {
                    anchored.push(p.scroller.0);
                }
            }

            let l = &self.host.lists.map[&id];
            let over = l.overscan;
            let range = |a: f32, b: f32| -> std::ops::Range<u32> {
                if count == 0 || b <= 0.0 || a >= total || a > b {
                    return 0..0;
                }
                let first = l.item_at(a.max(0.0)) as u32;
                let last = l.item_at(b.min(total - 1e-3).max(0.0)) as u32;
                first..(last + 1).min(count)
            };
            let need = range(p.v0 - over * 0.5, p.v1 + over * 0.5);
            let reported = l.reported.clone();
            let covers = reported.start <= need.start && need.end <= reported.end;
            let next = if reported.end > count || (!need.is_empty() && !covers) {
                range(p.v0 - over, p.v1 + over)
            } else {
                reported.clone()
            };
            let keep_id = if keep == NIL {
                NIL
            } else {
                l.descs.get(keep as usize).map_or(NIL, |d| d.id)
            };
            // After a splice the event goes out even with the same range:
            // it names the item order (revision) its indices refer to.
            if next != reported || keep != l.keep || keep_id != l.keep_id || l.resend {
                let l = self.host.lists.map.get_mut(&id).unwrap();
                l.reported = next.clone();
                l.keep = keep;
                l.keep_id = keep_id;
                l.resend = false;
                let revision = l.revision;
                let mut e = self.event(out_kind::LIST_RANGE, list);
                e.a = next.start as f32;
                e.b = next.end as f32;
                e.x = if keep == NIL { -1.0 } else { keep as f32 };
                // f32-exact: the revision modulo 2^24.
                e.y = (revision & 0xFF_FFFF) as f32;
                e.key = keep_id;
                self.pending_events.push(e);
            }
        }
        self.host.lists.ids = ids;
        self.host.lists.anchored = anchored;
    }

    /// The item index of the list row that holds focus, or NIL.
    fn focused_row(&self, list: NodeId) -> u32 {
        let Some(mut cur) = self.focus else {
            return NIL;
        };
        while cur.is_node() {
            let parent = self.host.parent(cur);
            if parent == list {
                return if self.host.list_row_shown(list, cur) {
                    self.host.list_index[cur.index()]
                } else {
                    NIL
                };
            }
            cur = parent;
        }
        NIL
    }

    /// Reports a native scroll to a listening scroller.
    pub(crate) fn scroll_event(&mut self, id: NodeId, off: [f32; 2]) {
        if self.host.interaction(id).listeners & crate::events::mask::SCROLL != 0 {
            let mut e = self.event(out_kind::SCROLL, id);
            e.a = off[0];
            e.b = off[1];
            self.pending_events.push(e);
        }
    }
}
