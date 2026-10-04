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
use crate::mutation::{
    Align, Anchor, Item, ItemDesc, ItemTemplate, Jump, ListOp, ListPolicy as Policy, NIL, Template,
};
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
    /// The last load asked (`updateItems`, first and last item), and
    /// whether a batch since moved rows or changed one inside it.
    pub asked: Option<(u32, u32)>,
    pub rearm: bool,
    /// Held items (identities): arrived in the viewport while a
    /// placeholder stays there. They keep their placeholder extents and
    /// don't measure until released.
    pub held: Vec<u32>,
    /// The viewport and visible range last reported.
    sent: Option<ListViewport>,
    sent_visible: Option<(i64, i64, bool, bool)>,
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
            asked: None,
            rearm: false,
            held: Vec::new(),
            sent: None,
            sent_visible: None,
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

/// A scroller's anchor: the visually top edge of list item `index` of
/// `list` sits `delta` points below the viewport top, and layout holds
/// it there (`list` NIL: no list item was visible). `bottom`: that edge
/// is the item's bottom (the list's y axis points up in the view).
///
/// The anchor is captured (the visually top item) after a frame with
/// reader input, when the scroller has none, and every frame while
/// following; a batch moves it by the batch rule (`batch_anchor`), a jump
/// sets it, and an edge clamp rewrites its offset to where its item is.
/// Measurements, resizes and mounts don't: the scroll follows the anchor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Saved {
    pub list: u32,
    pub index: u32,
    pub delta: f32,
    pub bottom: bool,
    /// Within the policy's end threshold of the end, at the last frame.
    pub at_end: bool,
    /// Held at the end until reader input: stick-to-end following, or a
    /// jump to the end.
    pub following: bool,
    /// Set by a jump: held until reader input, through batches that keep
    /// its item (wherever it moves, however it changes).
    pub explicit: bool,
    /// An index jump's alignment: item `index` is aligned again at every
    /// layout until reader input.
    pub jump: Option<Align>,
    /// Reader input moved the viewport since the last capture.
    pub moved: bool,
    /// The reader's last scroll direction: -1 up, 1 down, 0 none since a
    /// jump or the mount. Loads extend that way.
    pub direction: i8,
}

impl Saved {
    const NONE: Saved = Saved {
        list: NIL,
        index: 0,
        delta: 0.0,
        bottom: false,
        at_end: false,
        following: false,
        explicit: false,
        jump: None,
        moved: false,
        direction: 0,
    };
}

/// What a list shows of its viewport (`readViewport`).
#[derive(Clone, Debug, PartialEq)]
pub struct ListViewport {
    /// Items meeting the viewport (empty: none).
    pub visible: Range<u32>,
    /// The anchor: item identity, index, and its visually top edge below
    /// the viewport top.
    pub anchor: Option<(u32, u32, f32)>,
    /// The list content offset at the viewport top.
    pub offset: f32,
    pub at_end: bool,
    pub following: bool,
    /// Identities of rows kept rendered for focus.
    pub pinned: Vec<u32>,
    /// Held items (rendered as placeholders), first to last.
    pub held: Range<u32>,
    /// Items the bridge keeps mounted (pinned rows besides).
    pub mounted: Range<u32>,
}

impl ListViewport {
    /// `LIST_VIEWPORT`'s payload (LE): mounted first and end, visible
    /// first and end, held first and end (u32 each, ends exclusive), the
    /// anchor's identity (NIL: none) and index (u32) and offset (f32),
    /// the content offset (f32), flags (u8: bit 0 at end, bit 1
    /// following), then the pinned count (u16) and identities (u32).
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(46 + 4 * self.pinned.len());
        let (id, index, offset) = self.anchor.unwrap_or((NIL, 0, 0.0));
        for v in [
            self.mounted.start,
            self.mounted.end,
            self.visible.start,
            self.visible.end,
            self.held.start,
            self.held.end,
            id,
            index,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.push(self.at_end as u8 | (self.following as u8) << 1);
        out.extend_from_slice(&(self.pinned.len() as u16).to_le_bytes());
        for p in &self.pinned {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out
    }
}

/// A placeholder still waiting for content: not loaded, not failed.
fn pending(d: &Item) -> bool {
    d.flags & (Item::LOADED | Item::FAILED) == 0
}

impl ListState {
    /// The extent from item `a`'s start to item `b`'s end.
    fn span_extent(&self, a: usize, b: usize) -> f32 {
        self.offset(b) + self.extents.size(b) - self.offset(a)
    }

    /// The islet rule's request for a viewport `v0..v1` of the list's
    /// content, `view` tall, with `lookahead` (negative: one viewport)
    /// and the reader's direction (0: none since a mount or a jump):
    /// the pending items meeting the window, extended toward the reader
    /// to at least a screen, then widened on each side over a run's rest
    /// no taller than a screen. None when no pending item meets it.
    pub fn request(&self, v0: f32, v1: f32, lookahead: f32, direction: i8) -> Option<(u32, u32)> {
        let n = self.items.len();
        let total = self.total();
        let view = (v1 - v0).max(0.0);
        let look = if lookahead < 0.0 { view } else { lookahead };
        let (w0, w1) = (v0 - look, v1 + look);
        if n == 0 || w1 <= 0.0 || w0 >= total {
            return None;
        }
        let meets = |i: usize| {
            let o = self.offset(i);
            o < w1 && o + self.extents.size(i) > w0
        };
        let lo = self.item_at(w0.max(0.0));
        let hi = self.item_at(w1.min(total - 1e-3).max(0.0));
        let mut base = (lo..=hi).filter(|&i| meets(i) && pending(&self.items[i]));
        let mut first = base.next()?;
        let mut last = base.next_back().unwrap_or(first);
        let is = |i: usize| pending(&self.items[i]);
        // At least a screen, one pending row at a time, the reader's way
        // (after a mount or a jump: the way the run continues, down when
        // both).
        let down = match direction {
            d if d > 0 => true,
            d if d < 0 => false,
            _ => last + 1 < n && is(last + 1) || !(first > 0 && is(first - 1)),
        };
        while self.span_extent(first, last) < view {
            if down && last + 1 < n && is(last + 1) {
                last += 1;
            } else if !down && first > 0 && is(first - 1) {
                first -= 1;
            } else {
                break;
            }
        }
        // Widening: a run's rest past the request, no taller than a
        // screen, joins it (scanned no further than a screen).
        let mut start = first;
        while start > 0 && is(start - 1) && self.span_extent(start - 1, first - 1) <= view {
            start -= 1;
        }
        if start < first && (start == 0 || !is(start - 1)) {
            first = start;
        }
        let mut end = last;
        while end + 1 < n && is(end + 1) && self.span_extent(last + 1, end + 1) <= view {
            end += 1;
        }
        if end > last && (end + 1 == n || !is(end + 1)) {
            last = end;
        }
        Some((first as u32, last as u32))
    }
}

/// A row's tag: it renders version `version` of item `item` of list
/// `list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowTag {
    pub list: u32,
    pub item: u32,
    pub version: u32,
}

/// Every list's state, the scroll anchors, and per-size text metrics.
/// Lists are few: a map by node id.
#[derive(Default)]
pub struct Lists {
    pub(crate) map: HashMap<u32, ListState>,
    /// Policies that are not the default, by scroller.
    pub(crate) policies: HashMap<u32, Policy>,
    /// Anchors by scroller.
    pub(crate) saved: HashMap<u32, Saved>,
    /// Jumps waiting for their batch's layout, by list, in order.
    pub(crate) jumps: Vec<(u32, Jump)>,
    /// `read` commands waiting for the next frame: (list, request).
    pub(crate) reads: Vec<(u32, u32)>,
    /// Rows tagged by item (`LIST_ROW2`), by row node.
    pub rows: HashMap<u32, RowTag>,
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

    /// The item index row `row` stands for by its tag, if it has one: the
    /// index of the tagged item in its list (NIL once the item is gone).
    pub(crate) fn tagged_index(&mut self, row: u32) -> Option<u32> {
        let tag = *self.rows.get(&row)?;
        let l = self.map.get_mut(&tag.list);
        Some(l.and_then(|l| l.index_of(tag.item)).unwrap_or(NIL))
    }

    /// Whether row `row`, at item index `index` of list `list`, may
    /// measure the item: untagged (`LIST_INDEX`), or tagged with the
    /// item's version. A row rendered for an older version shows older
    /// content: it is placed, but its height isn't the item's.
    pub fn row_measures(&self, list: u32, row: u32, index: u32) -> bool {
        let Some(t) = self.rows.get(&row) else {
            return true;
        };
        let item = self
            .map
            .get(&list)
            .and_then(|l| l.items.get(index as usize));
        t.list == list
            && item.is_some_and(|d| d.id == t.item && d.version == t.version)
            && !self.map[&list].held.contains(&t.item)
    }

    pub fn policy(&self, scroller: u32) -> Policy {
        self.policies.get(&scroller).copied().unwrap_or_default()
    }

    pub(crate) fn set_policy(&mut self, scroller: u32, policy: Policy) {
        if policy == Policy::default() {
            self.policies.remove(&scroller);
        } else {
            self.policies.insert(scroller, policy);
        }
    }

    /// Reader input scrolled `scroller`: its anchor ends any jump and is
    /// captured again after the frame.
    pub(crate) fn reader_scrolled(&mut self, scroller: u32, dy: f32) {
        if let Some(s) = self.saved.get_mut(&scroller) {
            s.moved = true;
            s.explicit = false;
            s.jump = None;
            if dy != 0.0 {
                s.direction = if dy < 0.0 { -1 } else { 1 };
            }
        }
    }

    /// Whether jumps wait for the next layout.
    pub(crate) fn jumping(&self) -> bool {
        !self.jumps.is_empty()
    }

    /// Forgets everything a freed node held.
    pub(crate) fn forget(&mut self, id: u32) {
        self.map.remove(&id);
        self.rows.remove(&id);
        self.rows.retain(|_, t| t.list != id);
        self.policies.remove(&id);
        self.saved.remove(&id);
        self.saved.retain(|_, s| s.list != id);
        self.jumps.retain(|j| j.0 != id);
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
    /// identity takes its measurement. Anchors follow.
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
        // A kept item keeps its measurement; an unmeasured one takes its
        // new descriptor's estimate.
        let extents: Vec<(f32, bool)> = new
            .iter()
            .map(|(d, flag)| match find(d.id) {
                Some((_, before, e, true)) if keep(&before, d, *flag) => (e, true),
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

    /// A batch re-arms list `id`'s last load when it moves rows (a splice
    /// or a move: indices may shift) or changes one inside it (an answer,
    /// maybe partial); updates elsewhere leave it (trace L4).
    pub(crate) fn rearm(&mut self, id: u32, ops: &[ListOp]) {
        let Some(l) = self.map.get_mut(&id) else {
            return;
        };
        let inside = |at: u32, k: u32| l.asked.is_some_and(|(a, b)| at <= b && at + k > a);
        let rearm = ops.iter().any(|op| match op {
            ListOp::Update { at, items } => inside(*at, (items.len() / Item::BYTES) as u32),
            ListOp::Move { count: 0, .. } => false,
            _ => true,
        });
        l.rearm |= rearm;
    }

    /// Releases held items `ids` of list `id`: each takes its own
    /// estimate (it kept its placeholder's) and measures from then on.
    pub(crate) fn release(&mut self, text: &mut TextEngine, id: u32, ids: &[u32]) {
        if ids.is_empty() {
            return;
        }
        let est = self.estimator(text, id);
        let Some(l) = self.map.get_mut(&id) else {
            return;
        };
        l.held.retain(|h| !ids.contains(h));
        let width = l.width;
        for &h in ids {
            if let Some(i) = l.index_of(h) {
                let e = if width.is_finite() {
                    est.of(&l.items[i as usize], width)
                } else {
                    est.fallback
                };
                l.extents.estimate(i as usize, e);
            }
        }
        l.sent = None;
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

    /// Item `i`'s visually top edge below the viewport top, and its
    /// height in the view.
    fn item_view(&self, l: &ListState, i: usize) -> (f32, f32) {
        let o = l.offset(i);
        let (a, b) = (self.view_y(o), self.view_y(o + l.extents.size(i)));
        (a.min(b), (b - a).abs())
    }

    /// Whether item `i` meets the visible part of the list.
    fn meets(&self, l: &ListState, i: usize) -> bool {
        let o = l.offset(i);
        o < self.v1 && o + l.extents.size(i) > self.v0
    }

    /// The items meeting the visible part of the list (empty: none).
    fn visible(&self, l: &ListState) -> Range<u32> {
        let total = l.total();
        if l.is_empty() || self.v1 <= 0.0 || self.v0 >= total || self.v0 > self.v1 {
            return 0..0;
        }
        // `item_at` gives a gap to the item above it: step past an item
        // that ends at or above the top, or starts at or below the
        // bottom.
        let mut first = l.item_at(self.v0.max(0.0));
        if !self.meets(l, first) {
            first += 1;
        }
        let mut last = l.item_at(self.v1.min(total - 1e-3).max(0.0));
        if last > first && !self.meets(l, last) {
            last -= 1;
        }
        if first > last || !self.meets(l, first) {
            return 0..0;
        }
        first as u32..last as u32 + 1
    }

    /// The anchor a capture takes: the visually top item (the list's
    /// near end, or its far end when the list's y axis points up), or
    /// none.
    fn capture(&self, l: &ListState, id: u32, visible: bool) -> Saved {
        if !visible {
            return Saved::NONE;
        }
        let total = l.total();
        let bottom = self.flipped();
        let mut index = if bottom {
            l.item_at(self.v1.min(total - 1e-3).max(0.0))
        } else {
            l.item_at(self.v0.max(0.0))
        };
        // While a placeholder waits in view, the topmost visible loaded
        // row holds (with none loaded, the topmost visible row).
        let vis = self.visible(l);
        if vis.clone().any(|i| pending(&l.items[i as usize])) {
            let loaded = |&i: &u32| l.items[i as usize].flags & Item::LOADED != 0;
            let top = if bottom {
                vis.rev().find(loaded)
            } else {
                vis.clone().find(loaded)
            };
            if let Some(i) = top {
                index = i as usize;
            }
        }
        let edge = l.offset(index) + if bottom { l.extents.size(index) } else { 0.0 };
        Saved {
            list: id,
            index: index as u32,
            delta: self.view_y(edge),
            bottom,
            ..Saved::NONE
        }
    }
}

/// The part of a list a batch's ops touch, known from the ops alone:
/// the first `lo` items and the last `suffix` keep their places. `at` is
/// the first structural change (a splice or a move), and `end` the end
/// of the structural part in the new list (`len - suffix` of its ops).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Span {
    pub lo: u32,
    pub suffix: u32,
    pub at: Option<u32>,
    pub end: u32,
}

impl Span {
    /// The span of `ops` over a list of `len` items, and the new length;
    /// None when they change nothing. Indices below the running minimum
    /// are untouched by every op so far, so each op's own indices bound
    /// it in the old list's terms (and the suffix likewise).
    pub(crate) fn of(len: u32, ops: &[ListOp]) -> Option<(Span, u32)> {
        let (mut lo, mut suffix, mut at, mut struct_suffix) = (u32::MAX, u32::MAX, None, u32::MAX);
        let mut n = len;
        for op in ops {
            let (first, end_before, after) = match op {
                ListOp::Splice { at, remove, items } => {
                    let k = items.len() as u32 / Item::BYTES as u32;
                    (*at, at + remove, n - remove + k)
                }
                ListOp::Move { from, count, to } => {
                    if *count == 0 || from == to {
                        continue;
                    }
                    (*from.min(to), from.max(to) + count, n)
                }
                ListOp::Update { at, items } => {
                    let k = items.len() as u32 / Item::BYTES as u32;
                    if k == 0 {
                        continue;
                    }
                    (*at, at + k, n)
                }
            };
            lo = lo.min(first);
            suffix = suffix.min(n - end_before);
            if !matches!(op, ListOp::Update { .. }) {
                at = Some(at.map_or(first, |a: u32| a.min(first)));
                struct_suffix = struct_suffix.min(n - end_before);
            }
            n = after;
        }
        if lo == u32::MAX {
            return None;
        }
        let suffix = suffix.min(len - lo.min(len)).min(n - lo.min(n));
        let end = n - struct_suffix.min(n);
        Some((
            Span {
                lo,
                suffix,
                at,
                end,
            },
            n,
        ))
    }
}

/// A list as a batch found it, for the batch rule (`Ui::batch_before`).
pub(crate) struct Before {
    scroller: u32,
    saved: Saved,
    span: Span,
    old_len: u32,
    /// Visible items, visually top first: index, descriptor, and their
    /// visually top edge below the viewport top.
    rows: Vec<(u32, Item, f32)>,
    /// The old items in the span.
    old: Vec<Item>,
    /// The first visible place and its top.
    place: (u32, f32),
}

/// Whether each of `seq` (distinct values) lies on some longest
/// increasing subsequence: ending[k] + starting[k] - 1 == longest.
pub(crate) fn on_any_lis(seq: &[u32]) -> Vec<bool> {
    let lengths = |seq: &mut dyn Iterator<Item = i64>| -> Vec<usize> {
        // tails[l]: the smallest tail of an increasing run of length l + 1.
        let mut tails: Vec<i64> = Vec::new();
        seq.map(|v| {
            let l = tails.partition_point(|&t| t < v);
            if l == tails.len() {
                tails.push(v);
            } else {
                tails[l] = v;
            }
            l + 1
        })
        .collect()
    };
    let ending = lengths(&mut seq.iter().map(|&v| v as i64));
    let mut starting = lengths(&mut seq.iter().rev().map(|&v| -(v as i64)));
    starting.reverse();
    let longest = ending.iter().copied().max().unwrap_or(0);
    (0..seq.len())
        .map(|k| ending[k] + starting[k] - 1 == longest)
        .collect()
}

/// Where alignment `align` puts the top of an item `h` tall in a
/// viewport `view` tall.
fn align_delta(align: Align, view: f32, h: f32) -> f32 {
    match align {
        Align::Start => 0.0,
        Align::Center => (view - h) * 0.5,
        Align::End => view - h,
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
                // A band covered at the top isn't viewport (`startInset`).
                let d = self.layouts.data(cur);
                let inset = self.host.lists.policy(cur.0).start_inset;
                let c = d.clip_box;
                let inset = inset.min(c.size.height);
                let view = Rect::new(
                    c.origin.x,
                    c.origin.y + inset,
                    c.size.width,
                    c.size.height - inset,
                );
                break (cur, view, offset, d.scroll_extent[1]);
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

    /// After a layout pass: pending jumps take their scrollers, then
    /// every scroller holds its anchor: at the end while following, its
    /// jump's item at the jump's alignment, else (`KeepVisible`) its
    /// anchor item's edge `delta` below the viewport top.
    pub(crate) fn restore_anchors(&mut self) {
        let window = self.laid_out.unwrap_or(Size::ZERO);
        self.take_jumps(window);
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
            let target = if s.following {
                self.layouts.data(sc).scroll_extent[1]
            } else {
                // Jumps hold under any mode; anchors only keep-visible.
                if s.list == NIL || (s.jump.is_none() && policy.mode == Anchor::None) {
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
                let i = s.index as usize;
                match s.jump {
                    Some(align) => {
                        let (top, h) = p.item_view(l, i);
                        p.scroll[1] + top - align_delta(align, p.viewport.size.height, h)
                    }
                    // The scroll that puts the item's edge `delta` below
                    // the viewport top.
                    None => {
                        let edge = l.offset(i) + if s.bottom { l.extents.size(i) } else { 0.0 };
                        p.scroll[1] + p.view_y(edge) - s.delta
                    }
                }
            };
            let cur = self.host.spatial[sc.index()].scroll;
            if (target - cur[1]).abs() > ANCHOR_EPS
                && let Some(off) = self.set_scroll(sc, cur[0], target)
            {
                self.scroll_event(sc, off);
            }
            // Clamped at an edge: the anchor takes its item's place (an
            // aligned jump keeps its alignment).
            let applied = self.host.spatial[sc.index()].scroll[1];
            if !s.following
                && s.jump.is_none()
                && (target - applied).abs() > ANCHOR_EPS
                && let Some(a) = self.host.lists.saved.get_mut(&scroller)
            {
                a.delta += target - applied;
            }
        }
    }

    /// Applies the jumps waiting for this layout: each takes its list's
    /// scroller (an offset jump scrolls as the reader does).
    fn take_jumps(&mut self, window: Size) {
        for (list, jump) in std::mem::take(&mut self.host.lists.jumps) {
            let Some(p) = self.list_placement(NodeId(list), window) else {
                continue;
            };
            let sc = p.scroller;
            if !sc.is_node() {
                continue;
            }
            let end_mode = self.host.lists.policy(sc.0).mode == Anchor::StickToEnd;
            let Some(l) = self.host.lists.map.get_mut(&list) else {
                continue;
            };
            let last = l.len().saturating_sub(1);
            let (index, align) = match jump {
                // The last item aligned at the end of an end list is the
                // end: it follows.
                Jump::Index(i, Align::End) if end_mode && i >= last => {
                    self.jump_to_end(list, sc, end_mode, window);
                    continue;
                }
                Jump::Index(i, a) => (i.min(last), a),
                Jump::Item(id, a) => match l.index_of(id) {
                    Some(i) => (i, a),
                    None => continue,
                },
                Jump::End => {
                    self.jump_to_end(list, sc, end_mode, window);
                    continue;
                }
                Jump::Read => continue,
                Jump::Offset(y) => {
                    let target = p.scroll[1] + p.view_y(y as f32);
                    let x = self.host.spatial[sc.index()].scroll[0];
                    if let Some(off) = self.scroll_to(sc, x, target) {
                        self.scroll_event(sc, off);
                    }
                    continue;
                }
            };
            if l.is_empty() {
                continue;
            }
            let s = Saved {
                list,
                index,
                bottom: p.flipped(),
                explicit: true,
                jump: Some(align),
                ..Saved::NONE
            };
            self.host.lists.saved.insert(sc.0, s);
        }
    }

    /// The end: an end list follows it; a start list scrolls there once,
    /// its anchor then the item at the top, held explicitly.
    fn jump_to_end(&mut self, list: u32, sc: NodeId, end_mode: bool, window: Size) {
        if end_mode {
            let s = self.host.lists.saved.entry(sc.0).or_insert(Saved::NONE);
            (s.following, s.explicit, s.jump, s.moved) = (true, false, None, false);
            s.direction = 0;
            return;
        }
        let [x, _] = self.host.spatial[sc.index()].scroll;
        let max = self.layouts.data(sc).scroll_extent[1];
        if let Some(off) = self.set_scroll(sc, x, max) {
            self.scroll_event(sc, off);
        }
        let (Some(p), Some(l)) = (
            self.list_placement(NodeId(list), window),
            self.host.lists.get(list),
        ) else {
            return;
        };
        let visible = !l.is_empty() && !p.visible(l).is_empty();
        let s = Saved {
            explicit: true,
            ..p.capture(l, list, visible)
        };
        self.host.lists.saved.insert(sc.0, s);
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
                let sc = p.scroller.0;
                let policy = self.host.lists.policy(sc);
                let at_end = p.scroll[1] >= p.max_scroll - policy.end_threshold;
                let old = self.host.lists.saved.get(&sc).copied();
                // Another list's anchor holds this scroller: it decides.
                let other = old.is_some_and(|s| {
                    s.list != NIL
                        && s.list != id
                        && !s.moved
                        && self.host.lists.map.contains_key(&s.list)
                });
                if !other {
                    // The anchor stays unless reader input moved the
                    // viewport or the end holds instead.
                    let holds = old.is_some_and(|s| {
                        !s.moved && !s.following && s.list == id && s.index < count
                    });
                    let mut s = match old {
                        Some(s) if holds => s,
                        _ => p.capture(l, id, visible),
                    };
                    s.at_end = at_end;
                    s.following = match old {
                        None => policy.mode == Anchor::StickToEnd,
                        Some(o) if o.moved => policy.mode == Anchor::StickToEnd && at_end,
                        Some(o) => o.following,
                    };
                    self.host.lists.saved.insert(sc, s);
                    if visible {
                        anchored.push(sc);
                    }
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
            let v2 = l.v2;
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
            if v2 {
                self.sync_loading(id, &p);
            }
        }
        self.host.lists.ids = ids;
        self.host.lists.anchored = anchored;
        self.answer_reads();
    }

    /// The lists contract's per-frame work for list `id` in placement
    /// `p`: held rows that left the viewport, or whose placeholders are
    /// all gone, apply; the islet request goes to `updateItems` when it
    /// is new or re-armed; the viewport and the visible range are
    /// reported when they changed.
    fn sync_loading(&mut self, id: u32, p: &Placement) {
        let list = NodeId(id);
        let listeners = self.host.interaction(list).listeners;
        let l = &self.host.lists.map[&id];
        let vis = p.visible(l);
        if !l.held.is_empty() {
            let waiting = vis.clone().any(|i| pending(&l.items[i as usize]));
            let shown = |h: u32| vis.clone().any(|i| l.items[i as usize].id == h);
            let gone: Vec<u32> = (l.held.iter().copied())
                .filter(|&h| !waiting || !shown(h))
                .collect();
            if !gone.is_empty() {
                self.host.lists.release(&mut self.text, id, &gone);
                self.host.mark_layout(list);
            }
        }
        if listeners & crate::events::mask::UPDATE_ITEMS != 0 {
            let direction = (self.host.lists.saved.get(&p.scroller.0)).map_or(0, |s| s.direction);
            let l = &self.host.lists.map[&id];
            let request = l.request(p.v0, p.v1, l.lookahead, direction);
            let asked = l.asked;
            if let Some((a, b)) = request
                && (l.rearm || !asked.is_some_and(|(x, y)| x <= a && b <= y))
            {
                let l = self.host.lists.map.get_mut(&id).unwrap();
                l.asked = Some((a, b));
                l.rearm = false;
                let mut e = self.event(out_kind::CALL, list);
                e.key = crate::events::list_slot::UPDATE_ITEMS;
                e.payload = vec![1];
                e.payload.extend_from_slice(&a.to_le_bytes());
                e.payload.extend_from_slice(&b.to_le_bytes());
                self.pending_events.push(e);
            }
        }
        let Some(v) = self.list_viewport(list) else {
            return;
        };
        let l = self.host.lists.map.get_mut(&id).unwrap();
        let visible = (
            v.visible.start as i64,
            v.visible.end as i64 - 1,
            v.at_end,
            v.following,
        );
        let report = l.sent.as_ref() != Some(&v);
        let notify = l.sent_visible != Some(visible);
        if report {
            l.sent = Some(v.clone());
            let revision = l.revision;
            let mut e = self.event(out_kind::LIST_VIEWPORT, list);
            (e.x, e.y) = (v.mounted.start as f32, v.mounted.end as f32);
            (e.a, e.b) = (v.visible.start as f32, v.visible.end as f32);
            e.revision = revision;
            e.payload = v.bytes();
            self.pending_events.push(e);
        }
        if notify && listeners & crate::events::mask::VISIBLE_CHANGE != 0 {
            let l = self.host.lists.map.get_mut(&id).unwrap();
            l.sent_visible = Some(visible);
            let mut e = self.event(out_kind::CALL, list);
            e.key = crate::events::list_slot::VISIBLE_CHANGE;
            let (first, last) = if v.visible.is_empty() {
                (0, -1)
            } else {
                (visible.0, visible.1)
            };
            e.payload.extend_from_slice(&(first as i32).to_le_bytes());
            e.payload.extend_from_slice(&(last as i32).to_le_bytes());
            e.payload.push(v.at_end as u8 | (v.following as u8) << 1);
            self.pending_events.push(e);
        }
    }

    /// Answers the `read` commands that waited for this frame.
    fn answer_reads(&mut self) {
        for (list, request) in std::mem::take(&mut self.host.lists.reads) {
            let mut e = self.event(out_kind::LIST_VIEWPORT, NodeId(list));
            e.key = request;
            match self.list_viewport(NodeId(list)) {
                Some(v) => {
                    (e.x, e.y) = (v.mounted.start as f32, v.mounted.end as f32);
                    (e.a, e.b) = (v.visible.start as f32, v.visible.end as f32);
                    e.revision = self.host.lists.get(list).map_or(0, |l| l.revision);
                    e.payload = v.bytes();
                }
                None => e.revision = NIL,
            }
            self.pending_events.push(e);
        }
    }

    /// What the batch rule needs before `ops` apply to list `list`: its
    /// scroller's anchor (unless following, when the end holds), the
    /// visible rows with their tops, and the touched span's old items.
    /// None when the anchor isn't this list's or nothing is visible.
    pub(crate) fn batch_before(&self, list: u32, ops: &[ListOp]) -> Option<Before> {
        let l = self.host.lists.get(list)?;
        let p = self.list_placement(NodeId(list), self.laid_out?)?;
        let saved = *self.host.lists.saved.get(&p.scroller.0)?;
        if !p.scroller.is_node() || saved.list != list || saved.following {
            return None;
        }
        let (span, _) = Span::of(l.len(), ops)?;
        let vis = p.visible(l);
        if vis.is_empty() {
            return None;
        }
        let mut rows: Vec<(u32, Item, f32)> = vis
            .clone()
            .map(|i| (i, l.items[i as usize], p.item_view(l, i as usize).0))
            .collect();
        if p.flipped() {
            rows.reverse();
        }
        let old_len = l.len();
        let old = l.items[span.lo as usize..(old_len - span.suffix) as usize].to_vec();
        let place = span
            .at
            .map_or(vis.start, |at| at.max(vis.start))
            .min(old_len);
        let top = if (place as usize) < l.items.len() {
            p.item_view(l, place as usize).0
        } else {
            p.view_y(l.total())
        };
        Some(Before {
            scroller: p.scroller.0,
            saved,
            span,
            old_len,
            rows,
            old,
            place: (place, top),
        })
    }

    /// The batch rule, once the batch applied (`docs/contracts/lists.md`):
    /// an explicit anchor keeps its item while it survives; else the
    /// anchor stays when no visible row was removed or changed and it
    /// stayed in order; else the topmost visible row that stayed and is
    /// unchanged holds its place (loaded rows first); else the first
    /// visible place does. A row stayed outside the span, or on any
    /// longest increasing subsequence of the span's surviving items;
    /// it is unchanged when its identity survives with the same version,
    /// loaded and failed flags.
    /// List `list`'s visible items before a batch, for the hold rule:
    /// identity, flags, extent, measured.
    pub(crate) fn shown_items(&self, list: u32) -> Vec<(u32, u8, f32, bool)> {
        let (Some(l), Some(size)) = (self.host.lists.get(list), self.laid_out) else {
            return Vec::new();
        };
        let Some(p) = self.list_placement(NodeId(list), size) else {
            return Vec::new();
        };
        p.visible(l)
            .map(|i| {
                let (d, i) = (&l.items[i as usize], i as usize);
                (d.id, d.flags, l.extents.size(i), l.extents.is_measured(i))
            })
            .collect()
    }

    /// The hold rule, once a batch applied (`lists.md`, "Loading without
    /// layout shifts"): rows that arrived (unloaded to loaded) in the
    /// viewport are held while a pending placeholder stays there,
    /// keeping their placeholder extents; once none stays, every held row
    /// applies. Returns the rows released.
    pub(crate) fn hold(&mut self, list: u32, shown: &[(u32, u8, f32, bool)]) -> Vec<u32> {
        let Some(l) = self.host.lists.map.get_mut(&list) else {
            return Vec::new();
        };
        let mut arrived = Vec::new();
        let mut waiting = false;
        for &(id, flags, extent, measured) in shown {
            let Some(i) = l.index_of(id) else { continue };
            let now = l.items[i as usize];
            waiting |= pending(&now);
            if flags & Item::LOADED == 0 && now.flags & Item::LOADED != 0 {
                arrived.push((i as usize, id, extent, measured));
            }
        }
        if waiting {
            for (i, id, extent, measured) in arrived {
                if measured {
                    l.extents.measure(i, extent);
                } else {
                    l.extents.estimate(i, extent);
                }
                if !l.held.contains(&id) {
                    l.held.push(id);
                }
            }
            return Vec::new();
        }
        let released = std::mem::take(&mut l.held);
        l.held = released.clone();
        self.host.lists.release(&mut self.text, list, &released);
        released
    }

    pub(crate) fn batch_anchor(&mut self, list: u32, b: Before, released: &[u32]) {
        let Some(l) = self.host.lists.get(list) else {
            return;
        };
        let new_len = l.len();
        let Span {
            lo,
            suffix,
            at,
            end,
        } = b.span;
        let (old_end, new_end) = (b.old_len - suffix, new_len - suffix);
        // The span's new items by identity: (id, index), sorted.
        let mut found: Vec<(u32, u32)> = (lo..new_end)
            .filter(|&j| l.items[j as usize].id != NIL)
            .map(|j| (l.items[j as usize].id, j))
            .collect();
        found.sort_unstable();
        let map = |i: u32| -> Option<u32> {
            if i < lo {
                Some(i)
            } else if i >= old_end {
                Some(i + new_len - b.old_len)
            } else {
                let id = b.old[(i - lo) as usize].id;
                (id != NIL)
                    .then(|| found.binary_search_by_key(&id, |f| f.0).ok())
                    .flatten()
                    .map(|k| found[k].1)
            }
        };
        let survivors: Vec<(u32, u32)> = (lo..old_end).filter_map(|i| Some((i, map(i)?))).collect();
        let on = on_any_lis(&survivors.iter().map(|s| s.1).collect::<Vec<_>>());
        let stayed = |i: u32| {
            if i < lo || i >= old_end {
                return true;
            }
            survivors
                .binary_search_by_key(&i, |s| s.0)
                .is_ok_and(|k| on[k])
        };
        const STATE: u8 = Item::LOADED | Item::FAILED;
        // A row released in this batch changed, though its descriptor
        // arrived earlier (trace L5).
        let unchanged = |i: u32, before: &Item| {
            !released.contains(&before.id)
                && map(i).is_some_and(|j| {
                    let now = &l.items[j as usize];
                    now.version == before.version && now.flags & STATE == before.flags & STATE
                })
        };
        let s = b.saved;
        let keep = |index: u32, delta: f32, explicit: bool| Saved {
            index,
            delta,
            explicit,
            jump: if explicit { s.jump } else { None },
            ..s
        };
        let next = if let Some(j) = map(s.index).filter(|_| s.explicit) {
            keep(j, s.delta, true)
        } else if let Some(j) = map(s.index)
            .filter(|_| stayed(s.index) && b.rows.iter().all(|(i, d, _)| unchanged(*i, d)))
        {
            keep(j, s.delta, false)
        } else {
            let pick = |loaded: bool| {
                b.rows.iter().find(|(i, d, _)| {
                    stayed(*i) && unchanged(*i, d) && (!loaded || d.flags & Item::LOADED != 0)
                })
            };
            match pick(true).or_else(|| pick(false)) {
                Some(&(i, _, top)) => keep(map(i).unwrap(), top, false),
                None if new_len == 0 => keep(0, 0.0, false),
                None => {
                    let (place, top) = b.place;
                    let bound = if at.is_some() { end } else { place };
                    keep(place.min(bound).min(new_len - 1), top, false)
                }
            }
        };
        if let Some(cur) = self.host.lists.saved.get_mut(&b.scroller) {
            *cur = Saved {
                at_end: cur.at_end,
                moved: cur.moved,
                ..next
            };
        }
    }

    /// What list `list` shows of its viewport, from the current layout
    /// and scroll (`readViewport`); None for a node that isn't a list.
    pub fn list_viewport(&self, list: NodeId) -> Option<ListViewport> {
        let l = self.host.lists.get(list.0)?;
        let p = self.list_placement(list, self.laid_out.unwrap_or(Size::ZERO))?;
        let saved = self.host.lists.saved.get(&p.scroller.0).copied();
        let anchor = saved
            .filter(|s| p.scroller.is_node() && s.list == list.0 && s.index < l.len())
            .map(|s| {
                let i = s.index as usize;
                (l.items[i].id, s.index, p.item_view(l, i).0)
            });
        let threshold = self.host.lists.policy(p.scroller.0).end_threshold;
        let keep = self.focused_row(list);
        let pinned = l
            .items
            .get(keep as usize)
            .map(|d| d.id)
            .filter(|&id| id != NIL);
        // Held rows are visible ones (the others are released).
        let visible = p.visible(l);
        let held: Vec<u32> = if l.held.is_empty() {
            Vec::new()
        } else {
            visible
                .clone()
                .filter(|&i| l.held.contains(&l.items[i as usize].id))
                .collect()
        };
        let held = match (held.first(), held.last()) {
            (Some(&a), Some(&b)) => a..b + 1,
            _ => 0..0,
        };
        Some(ListViewport {
            visible,
            anchor,
            offset: p.v0,
            at_end: p.scroller.is_node() && p.scroll[1] >= p.max_scroll - threshold,
            following: saved.is_some_and(|s| s.following),
            pinned: pinned.into_iter().collect(),
            held,
            mounted: l.reported.clone(),
        })
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
