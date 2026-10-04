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
use crate::mutation::{Anchor, Item, ItemDesc, ItemTemplate, ListOp, NIL, Template};
use crate::text::TextEngine;
use crate::text::paragraph::{SpanStyle, TextSpec, TextStyle};
use craie_core::Affine;

/// Upper bound on a list's item count: bounds the per-item stores
/// (about 19 bytes per item), and keeps item indices exact in the f32
/// fields of range events.
pub const MAX_ITEMS: u32 = 1 << 24;

/// The identities of a list's items (NIL excluded), sorted. Ids come
/// from the public wire, so they are adversarial input: a sorted vector
/// has no hash to aim collisions at. Membership is a binary search;
/// an update is one merge pass, O(n + k log k), in line with the O(n)
/// extents rebuild every splice already does. 4 bytes per item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IdIndex(Vec<u32>);

impl IdIndex {
    pub fn contains(&self, id: u32) -> bool {
        self.0.binary_search(&id).is_ok()
    }

    pub fn as_slice(&self) -> &[u32] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The index of `ids` (NIL dropped); duplicates stay (a caller that
    /// requires uniqueness checks `has_duplicates`).
    pub fn build(ids: impl IntoIterator<Item = u32>) -> IdIndex {
        let mut v: Vec<u32> = ids.into_iter().filter(|&i| i != NIL).collect();
        v.sort_unstable();
        IdIndex(v)
    }

    pub fn has_duplicates(&self) -> bool {
        self.0.windows(2).any(|w| w[0] == w[1])
    }

    /// Removes `removed` (present ids) and adds `added` (absent ids);
    /// NIL is ignored in both. With n ids, r removed, and a added:
    /// O(r·n + a·n) memmoves for a few ids (r + a <= 16), else one linear
    /// merge, O(n + r log r + a log a): the old index, the sorted removals,
    /// and the sorted additions are scanned together once.
    pub fn update(&mut self, removed: &[u32], added: &[u32]) {
        let removed: Vec<u32> = removed.iter().copied().filter(|&i| i != NIL).collect();
        let added: Vec<u32> = added.iter().copied().filter(|&i| i != NIL).collect();
        if removed.len() + added.len() <= 16 {
            // A few ids: in place (one memmove each).
            for r in removed {
                if let Ok(p) = self.0.binary_search(&r) {
                    self.0.remove(p);
                }
            }
            for a in added {
                if let Err(p) = self.0.binary_search(&a) {
                    self.0.insert(p, a);
                }
            }
            return;
        }
        let gone = IdIndex::build(removed);
        let new = IdIndex::build(added);
        let mut out = Vec::with_capacity(self.0.len() + new.len() - gone.len().min(self.0.len()));
        let (mut g, mut a) = (
            gone.0.iter().copied().peekable(),
            new.0.iter().copied().peekable(),
        );
        for x in self.0.iter().copied() {
            // Skip removals below x (absent ids), drop x if removed.
            while g.next_if(|&r| r < x).is_some() {}
            if g.next_if_eq(&x).is_some() {
                continue;
            }
            while let Some(y) = a.next_if(|&y| y < x) {
                out.push(y);
            }
            out.push(x);
        }
        out.extend(a);
        self.0 = out;
    }
}

/// Sample text for per-size metrics: a mix of letters, digits, and
/// spaces close to running prose.
const SAMPLE: &str = "The quick brown fox jumps over the lazy dog, 0123456789 times.";

pub struct ListState {
    /// `LIST_CONFIG`'s templates (shaped text), used unless `v2`.
    pub templates: Vec<ItemTemplate>,
    /// `LIST_CONFIG2`'s templates, used when `v2`.
    pub templates2: Vec<Template>,
    pub v2: bool,
    /// Lookahead (logical points; negative: one viewport height) and
    /// retain (viewport heights), from `LIST_CONFIG2`.
    pub lookahead: f32,
    pub retain: f32,
    /// Template epoch: a new one drops every measurement.
    pub epoch: u32,
    /// Measurements are stale (a new epoch): the next estimate drops them.
    pub forget: bool,
    pub items: Vec<Item>,
    /// The identities in `items` (NIL excluded): each appears once, which
    /// validation checks against this index.
    pub ids: IdIndex,
    /// (identity, index) by identity, for rows tagged by item; rebuilt on
    /// demand after the items change.
    positions: Vec<(u32, u32)>,
    positions_ok: bool,
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
            templates2: Vec::new(),
            v2: false,
            lookahead: -1.0,
            retain: 3.0,
            epoch: 0,
            forget: false,
            items: Vec::new(),
            ids: IdIndex::default(),
            positions: Vec::new(),
            positions_ok: false,
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
        self.items.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The index of the item with identity `id`. O(log n), after an
    /// O(n log n) rebuild when the items changed.
    pub fn index_of(&mut self, id: u32) -> Option<u32> {
        if id == NIL {
            return None;
        }
        if !self.positions_ok {
            self.positions.clear();
            self.positions.extend(
                self.items
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.id != NIL)
                    .map(|(i, d)| (d.id, i as u32)),
            );
            self.positions.sort_unstable();
            self.positions_ok = true;
        }
        self.positions
            .binary_search_by_key(&id, |p| p.0)
            .ok()
            .map(|p| self.positions[p].1)
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

/// A scroller's captured anchor: the visually top edge of list item
/// `index` of `list` sat `delta` points below the viewport top (`list`
/// NIL: no list item was visible). `bottom`: that edge is the item's
/// bottom (the list's y axis points up in the view). `at_end`: the
/// scroller was at its end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Saved {
    pub list: u32,
    pub index: u32,
    pub delta: f32,
    pub bottom: bool,
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
        if l.templates != templates || l.v2 {
            l.templates = templates.to_vec();
            l.v2 = false;
            l.stale = true;
        }
    }

    /// `LIST_CONFIG2`: as `configure`, with the contract's templates; a
    /// new epoch drops every measurement.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn configure2(
        &mut self,
        id: u32,
        overscan: f32,
        lookahead: f32,
        retain: f32,
        fallback: f32,
        epoch: u32,
        templates: &[Template],
    ) {
        let l = self.map.entry(id).or_default();
        (l.overscan, l.lookahead, l.retain) = (overscan, lookahead, retain);
        if l.fallback != fallback {
            l.fallback = fallback;
            l.stale = true;
        }
        if l.templates2 != templates || !l.v2 {
            l.templates2 = templates.to_vec();
            l.v2 = true;
            l.stale = true;
        }
        if l.epoch != epoch {
            l.epoch = epoch;
            l.forget = true;
            l.stale = true;
        }
    }

    /// How list `id` estimates its items now (shaping v1 templates'
    /// samples once).
    fn estimator(&mut self, text: &mut TextEngine, id: u32) -> Estimator {
        let Some(l) = self.map.get(&id) else {
            return Estimator::default();
        };
        let (v2, fallback) = (l.v2, l.fallback);
        let v1: Vec<ItemTemplate> = if v2 { Vec::new() } else { l.templates.clone() };
        let templates2 = if v2 { l.templates2.clone() } else { Vec::new() };
        let v1 = v1
            .into_iter()
            .map(|t| {
                let m = if t.font_size > 0.0 {
                    self.metrics(text, t.font_size)
                } else {
                    (1.0, 0.0)
                };
                (t, m)
            })
            .collect();
        Estimator {
            v1,
            v2: templates2,
            fallback,
        }
    }

    /// Replaces items `at..at + remove` with `items` (`LIST_SPLICE`). An
    /// item that moves within the splice marked `unchanged` (same id
    /// removed and inserted) keeps its extent and measurement; other new
    /// items are estimated at the list's width when it is known (else at
    /// the next layout). A captured anchor follows its item: to its new
    /// place when it moved, to the splice start only when it was removed.
    pub(crate) fn splice(
        &mut self,
        text: &mut TextEngine,
        id: u32,
        at: u32,
        remove: u32,
        items: &[u8],
    ) {
        let est = self.estimator(text, id);
        let l = self.map.entry(id).or_default();
        let n = l.len();
        // Validation guarantees the range; clamp rather than panic.
        debug_assert!(at <= n && remove <= n - at.min(n), "splice out of range");
        let at = at.min(n);
        let remove = remove.min(n - at);
        let new: Vec<(Item, bool)> = ItemDesc::iter(items)
            .map(|d| {
                let item = Item {
                    id: d.id,
                    version: 0,
                    template: d.template,
                    flags: Item::LOADED,
                    arg: d.text_len,
                };
                (item, d.unchanged)
            })
            .collect();
        // Only the bridge knows the content is the same (the same object
        // moved): then it keeps its extent. Otherwise it is estimated
        // again (its row, if rendered, measures it).
        let keep = |before: &Item, after: &Item, unchanged: bool| {
            unchanged && before.template == after.template
        };
        self.replace(&est, id, at, remove, &new, keep);
        let l = self.map.get_mut(&id).unwrap();
        l.revision = l.revision.wrapping_add(1);
    }

    /// Replaces items `at..at + remove` with `new` (an item and a flag
    /// for `keep`): an item that `keep`s the removed one with its
    /// identity takes its extent and measurement. Anchors follow.
    fn replace(
        &mut self,
        est: &Estimator,
        id: u32,
        at: u32,
        remove: u32,
        new: &[(Item, bool)],
        keep: impl Fn(&Item, &Item, bool) -> bool,
    ) {
        let l = self.map.get_mut(&id).unwrap();
        let range = at as usize..(at + remove) as usize;
        let width = l.width;
        // Removed items by identity, sorted: (id, item, extent, measured).
        // Binary search, no hashing of wire ids.
        let mut removed: Vec<(u32, Item, f32, bool)> = range
            .clone()
            .filter(|&i| l.items[i].id != NIL)
            .map(|i| {
                (
                    l.items[i].id,
                    l.items[i],
                    l.extents.size(i),
                    l.extents.is_measured(i),
                )
            })
            .collect();
        removed.sort_unstable_by_key(|r| r.0);
        let find = |id: u32| {
            removed
                .binary_search_by_key(&id, |r| r.0)
                .ok()
                .map(|p| removed[p])
        };
        let extents: Vec<(f32, bool)> = new
            .iter()
            .map(|(d, flag)| match find(d.id) {
                Some((_, before, e, m)) if keep(&before, d, *flag) => (e, m),
                _ if width.is_finite() => (est.of(d, width), false),
                _ => (est.fallback, false),
            })
            .collect();
        let gone: Vec<u32> = l.items[range.clone()].iter().map(|d| d.id).collect();
        let added: Vec<u32> = new.iter().map(|(d, _)| d.id).collect();
        l.ids.update(&gone, &added);
        // Where each saved anchor's item went: its new place if the
        // splice re-inserted it, else the splice start.
        let anchor_to = |old: u32| {
            let id = l.items[old as usize].id;
            if id == NIL {
                return at;
            }
            added
                .iter()
                .position(|&a| a == id)
                .map_or(at, |k| at + k as u32)
        };
        let shift = new.len() as i64 - remove as i64;
        for s in self.saved.values_mut().filter(|s| s.list == id) {
            if s.index >= at + remove {
                s.index = (s.index as i64 + shift) as u32;
            } else if s.index >= at {
                s.index = anchor_to(s.index);
            }
        }
        l.items.splice(range.clone(), new.iter().map(|(d, _)| *d));
        l.extents.splice_items(range, extents);
        if !width.is_finite() {
            l.stale = true;
        }
        l.positions_ok = false;
        l.resend = true;
    }

    /// Applies a validated `LIST_PATCH`'s ops and takes list `id` to
    /// revision `next`. An item keeps its measurement while it keeps its
    /// identity and version: through moves, and through a splice that
    /// removes and re-inserts it.
    pub(crate) fn patch(&mut self, text: &mut TextEngine, id: u32, next: u32, ops: &[ListOp]) {
        let est = self.estimator(text, id);
        self.map.entry(id).or_default();
        for op in ops {
            match op {
                ListOp::Splice { at, remove, items } => {
                    let new: Vec<(Item, bool)> = Item::iter(items).map(|d| (d, false)).collect();
                    let keep = |before: &Item, after: &Item, _| before.version == after.version;
                    self.replace(&est, id, *at, *remove, &new, keep);
                }
                ListOp::Move { from, count, to } => self.move_items(id, *from, *count, *to),
                ListOp::Update { at, items } => {
                    let l = self.map.get_mut(&id).unwrap();
                    let width = l.width;
                    for (k, d) in Item::iter(items).enumerate() {
                        let i = *at as usize + k;
                        let before = std::mem::replace(&mut l.items[i], d);
                        // A new version may lay out differently: it takes
                        // its new estimate until its row measures it.
                        if before.version != d.version || !l.extents.is_measured(i) {
                            let e = if width.is_finite() {
                                est.of(&d, width)
                            } else {
                                est.fallback
                            };
                            l.extents.estimate(i, e);
                        }
                    }
                    if !width.is_finite() {
                        l.stale = true;
                    }
                    l.resend = true;
                }
            }
        }
        let l = self.map.get_mut(&id).unwrap();
        l.revision = next;
    }

    /// Moves items `from..from + count` to `to` (an index without them):
    /// extents, measurements and captured anchors go with them.
    fn move_items(&mut self, id: u32, from: u32, count: u32, to: u32) {
        let l = self.map.get_mut(&id).unwrap();
        let (f, c, t) = (from as usize, count as usize, to as usize);
        if c == 0 || f == t {
            return;
        }
        let moved: Vec<Item> = l.items.drain(f..f + c).collect();
        l.items.splice(t..t, moved);
        let ext: Vec<(f32, bool)> = (f..f + c)
            .map(|i| (l.extents.size(i), l.extents.is_measured(i)))
            .collect();
        l.extents.splice_items(f..f + c, []);
        l.extents.splice_items(t..t, ext);
        for s in self.saved.values_mut().filter(|s| s.list == id) {
            s.index = moved_index(s.index, from, count, to);
        }
        l.positions_ok = false;
        l.resend = true;
    }

    /// (average advance, line height) at `font_size`, measured once by
    /// shaping a sample.
    fn metrics(&mut self, text: &mut TextEngine, font_size: f32) -> (f32, f32) {
        *self.metrics.entry(font_size.to_bits()).or_insert_with(|| {
            let spans = [SpanStyle {
                start: 0,
                style: TextStyle {
                    size: font_size,
                    ..TextStyle::default()
                },
            }];
            let p = text.layout_text(
                &TextSpec {
                    text: SAMPLE,
                    spans: &spans,
                },
                None,
            );
            let advance = p.width / SAMPLE.chars().count() as f32;
            (advance.max(0.01), p.height)
        })
    }

    /// Brings list `id`'s estimates up to date for content width
    /// `width`. A width change, or a new template epoch, forgets
    /// measurements (they were made at another width or with other
    /// templates); rendered rows measure again in the same pass.
    pub(crate) fn estimate(&mut self, text: &mut TextEngine, id: u32, width: f32) {
        let Some(l) = self.map.get(&id) else { return };
        let resized = l.width != width;
        if !resized && !l.stale {
            return;
        }
        let est = self.estimator(text, id);
        let l = self.map.get_mut(&id).unwrap();
        let items = &l.items;
        l.extents
            .reestimate(resized || l.forget, |i| est.of(&items[i], width));
        l.width = width;
        l.stale = false;
        l.forget = false;
    }
}

/// Where item index `i` goes when items `from..from + count` move to
/// `to` (an index in the list without them).
pub fn moved_index(i: u32, from: u32, count: u32, to: u32) -> u32 {
    if (from..from + count).contains(&i) {
        return to + (i - from);
    }
    let j = if i >= from + count { i - count } else { i };
    if j >= to { j + count } else { j }
}

/// A list's estimates: shaped v1 templates with their metrics, or the
/// contract's templates, else the fallback; a numeric estimate wins.
#[derive(Default)]
struct Estimator {
    v1: Vec<(ItemTemplate, (f32, f32))>,
    v2: Vec<Template>,
    fallback: f32,
}

impl Estimator {
    fn of(&self, d: &Item, width: f32) -> f32 {
        if let Some(s) = d.size() {
            return s;
        }
        let t = d.template as usize;
        if let Some((tpl, m)) = self.v1.get(t) {
            return estimate(tpl, *m, d.arg, width);
        }
        self.v2
            .get(t)
            .map_or(self.fallback, |tpl| tpl.estimate(d.arg, width))
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
        // In f64 throughout, rounded once at the end: an f32 correction
        // of widely differing extents rounds away (1e6 -> 0.01 is -1e6).
        let gaps = l.items.len().saturating_sub(1) as f64 * gap as f64;
        if l.width == width && !l.stale {
            let mut t = l.extents.total_f64();
            for &(i, e) in rows {
                t += e as f64 - l.extents.size(i as usize) as f64;
            }
            return (t + gaps) as f32;
        }
        let est = self.estimator(text, id);
        let l = &self.map[&id];
        // Measurements hold at their own width only.
        let keep = l.width == width && !l.forget;
        let item = |i: usize| {
            if keep && l.extents.is_measured(i) {
                return l.extents.size(i);
            }
            est.of(&l.items[i], width)
        };
        let mut t: f64 = (0..l.items.len()).map(|i| item(i) as f64).sum();
        for &(i, e) in rows {
            t += e as f64 - item(i as usize) as f64;
        }
        (t + gaps) as f32
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
    /// Whether the list's y axis points up in the view (a flip on the
    /// list or an ancestor): the visual top is then the list's far end.
    fn flipped(&self) -> bool {
        self.to_view.0[3] < 0.0
    }

    /// Where list content offset `y` sits below the viewport top.
    fn view_y(&self, y: f32) -> f32 {
        let p = self
            .to_view
            .apply(Point::new(self.content[0], self.content[1] + y));
        p.y - self.scroll[1] - self.viewport.origin.y
    }
}

/// Distance under which an anchor correction is noise, not motion.
const ANCHOR_EPS: f32 = 1e-3;

impl crate::host::Host {
    /// Whether list row `row` is laid out and published: its index is in
    /// range, it is displayed, and no earlier displayed row of the list
    /// has the same index. Layout and accessibility share this rule.
    pub fn list_row_shown(&self, list: NodeId, row: NodeId) -> bool {
        let Some(l) = self.lists.get(list.0) else {
            return false;
        };
        let shown = |r: NodeId| {
            self.list_index[r.index()] < l.len() && self.style(r).display() != taffy::Display::None
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
        // A node's border box -> its parent's (as `scene_sync`).
        let local = |id: NodeId| {
            let d = self.layouts.data(id);
            Affine::translate(d.rect.origin.x, d.rect.origin.y)
                .mul(&self.host.spatial[id.index()].local(d.rect.size))
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
            if style.overflow().y == taffy::Overflow::Scroll {
                let d = self.layouts.data(cur);
                break (cur, d.clip_box, offset, d.scroll_extent[1]);
            }
            if style.overflow().x == taffy::Overflow::Scroll {
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
                    // The scroll that puts the item's edge `delta` below the
                    // viewport top.
                    let i = s.index as usize;
                    let edge = l.offset(i) + if s.bottom { l.extents.size(i) } else { 0.0 };
                    p.scroll[1] + p.view_y(edge) - s.delta
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
                    // The visually top item: the list's near end, or its
                    // far end when the list's y axis points up.
                    let bottom = p.flipped();
                    let index = if bottom {
                        l.item_at(p.v1.min(total - 1e-3).max(0.0))
                    } else {
                        l.item_at(p.v0.max(0.0))
                    };
                    let edge = l.offset(index) + if bottom { l.extents.size(index) } else { 0.0 };
                    Saved {
                        list: id,
                        index: index as u32,
                        delta: p.view_y(edge),
                        bottom,
                        at_end,
                    }
                } else {
                    Saved {
                        list: NIL,
                        index: 0,
                        delta: 0.0,
                        bottom: false,
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
                l.items.get(keep as usize).map_or(NIL, |d| d.id)
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
