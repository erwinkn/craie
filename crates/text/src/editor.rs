//! Owned plain-text editing (ARCHITECTURE.md §5, step 3b): one UTF-8
//! buffer, a selection of two cursors, and an IME preedit held in the
//! buffer, over the owned paragraph.
//!
//! The model follows Parley's `PlainEditor` (the E01 oracle for editing):
//! a cursor is a byte index at a cluster boundary plus an affinity (which
//! side's cluster it belongs to, so a soft line break has a caret at the
//! end of one line and at the start of the next); left and right move one
//! cluster in visual order; up and down keep a horizontal goal; word
//! moves and deletions use UAX #29 word boundaries. The preedit lives in
//! the buffer (`raw_text`); `text` excludes it.
//!
//! Layout goes through the `TextEngine`: an edit shapes once (counted in
//! `TextEngine::shapes`), a width change only rewraps. Geometry and
//! mapping read the paragraph's placements.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use crate::TextEngine;
use crate::fonts::emoji_presentation;
use crate::paragraph::{Paragraph, SpanStyle, TextSpec, TextStyle, VisualCluster, is_newline};

/// Which side of a cluster boundary a cursor belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Affinity {
    /// The cluster after the index (the default).
    #[default]
    Downstream,
    /// The cluster before the index.
    Upstream,
}

/// A caret position: a byte index at a cluster boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub index: u32,
    pub affinity: Affinity,
}

/// What a selection extends by when dragged.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum AnchorBase {
    #[default]
    Cluster,
    Word(Cursor, Cursor),
}

/// Anchor and focus; the focus moves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Selection {
    pub anchor: Cursor,
    pub focus: Cursor,
    /// Horizontal goal of repeated up/down moves.
    h_pos: Option<f32>,
    anchor_base: AnchorBase,
}

impl Selection {
    fn collapsed(c: Cursor) -> Selection {
        Selection {
            anchor: c,
            focus: c,
            ..Selection::default()
        }
    }

    fn new(anchor: Cursor, focus: Cursor) -> Selection {
        Selection {
            anchor,
            focus,
            ..Selection::default()
        }
    }

    pub fn is_collapsed(&self) -> bool {
        self.anchor.index == self.focus.index
    }

    /// The selected bytes, in order.
    pub fn text_range(&self) -> Range<u32> {
        let (a, b) = (self.anchor.index, self.focus.index);
        a.min(b)..a.max(b)
    }
}

/// A rectangle (x, y, width, height) in paragraph space.
pub type Rect = (f32, f32, f32, f32);

/// An editable plain-text buffer with one style.
pub struct Editor {
    buffer: String,
    /// The IME preedit's bytes in `buffer`, while composing.
    compose: Option<Range<u32>>,
    /// The IME may hide the caret.
    show_cursor: bool,
    selection: Selection,
    size: f32,
    width: Option<f32>,
    layout: Paragraph,
    /// Derived from `buffer` with the layout: clusters in logical order,
    /// and UAX #29 word-segment starts.
    clusters: Vec<Range<u32>>,
    words: Vec<u32>,
    shape_dirty: bool,
    wrap_dirty: bool,
    generation: u64,
}

impl Editor {
    pub fn new(size: f32) -> Editor {
        Editor {
            buffer: String::new(),
            compose: None,
            show_cursor: true,
            selection: Selection::default(),
            size,
            width: None,
            layout: Paragraph::default(),
            clusters: Vec::new(),
            words: Vec::new(),
            shape_dirty: true,
            wrap_dirty: false,
            generation: 0,
        }
    }

    // --- MARK: state ---

    /// The buffer with the preedit.
    pub fn raw_text(&self) -> &str {
        &self.buffer
    }

    /// The committed text: the buffer without the preedit.
    pub fn text(&self) -> String {
        match &self.compose {
            Some(c) => {
                let mut s = String::with_capacity(self.buffer.len());
                s.push_str(&self.buffer[..c.start as usize]);
                s.push_str(&self.buffer[c.end as usize..]);
                s
            }
            None => self.buffer.clone(),
        }
    }

    /// Whether the committed text (the buffer without the preedit) is
    /// `text`, without allocating.
    pub fn text_is(&self, text: &str) -> bool {
        match &self.compose {
            Some(c) => {
                let (a, b) = (
                    &self.buffer[..c.start as usize],
                    &self.buffer[c.end as usize..],
                );
                text.len() == a.len() + b.len() && text.starts_with(a) && text.ends_with(b)
            }
            None => self.buffer == text,
        }
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn raw_compose(&self) -> Option<Range<u32>> {
        self.compose.clone()
    }

    pub fn is_composing(&self) -> bool {
        self.compose.is_some()
    }

    /// The selected text, when not composing and not collapsed.
    pub fn selected_text(&self) -> Option<&str> {
        if self.is_composing() || self.selection.is_collapsed() {
            return None;
        }
        let r = self.selection.text_range();
        self.buffer.get(r.start as usize..r.end as usize)
    }

    /// Changes when the buffer, layout inputs, or selection change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn font_size(&self) -> f32 {
        self.size
    }

    /// Replaces the buffer; ends composing. The selection is clamped at
    /// the next layout.
    pub fn set_text(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
        self.compose = None;
        self.show_cursor = true;
        self.shape_dirty = true;
        self.generation += 1;
        // Valid at once (an edit before the next layout slices the buffer
        // with it); cluster boundaries follow at the next layout.
        self.clamp_selection();
    }

    pub fn set_font_size(&mut self, size: f32) {
        if self.size != size {
            self.size = size;
            self.shape_dirty = true;
            self.generation += 1;
        }
    }

    /// Sets the wrap width: a rewrap, not a reshape.
    pub fn set_width(&mut self, width: Option<f32>) {
        if self.width != width {
            self.width = width;
            self.wrap_dirty = true;
            self.generation += 1;
        }
    }

    // --- MARK: layout ---

    /// Whether `layout` is current.
    pub fn is_clean(&self) -> bool {
        !self.shape_dirty && !self.wrap_dirty
    }

    /// Brings the layout up to date: shapes after an edit, rewraps after
    /// a width change. Returns whether it shaped.
    pub fn refresh(&mut self, engine: &mut TextEngine) -> bool {
        if self.shape_dirty {
            let spans = [SpanStyle {
                start: 0,
                style: TextStyle {
                    size: self.size,
                    ..TextStyle::default()
                },
            }];
            let spec = TextSpec {
                text: &self.buffer,
                spans: &spans,
            };
            self.layout = engine.layout_text(&spec, self.width);
            self.clusters.clear();
            self.clusters
                .extend(self.layout.cluster_map().into_iter().map(|(t, _)| t));
            self.words.clear();
            self.words.extend(
                self.buffer
                    .split_word_bound_indices()
                    .map(|(i, _)| i as u32),
            );
            self.shape_dirty = false;
            self.wrap_dirty = false;
            self.clamp_selection();
            return true;
        }
        if self.wrap_dirty {
            engine.rewrap(&mut self.layout, self.width);
            self.wrap_dirty = false;
        }
        false
    }

    /// The layout; `refresh` first.
    pub fn layout(&self) -> &Paragraph {
        debug_assert!(self.is_clean(), "editor layout read while dirty");
        &self.layout
    }

    /// Moves both cursors onto the buffer: at most its length and on a
    /// character boundary, and on a cluster boundary when the layout is
    /// current. Drops the drag granularity and the vertical goal.
    fn clamp_selection(&mut self) {
        let clamp = |ed: &Editor, c: Cursor| {
            let mut i = c.index.min(ed.len());
            while !ed.buffer.is_char_boundary(i as usize) {
                i -= 1;
            }
            if ed.shape_dirty {
                Cursor {
                    index: i,
                    affinity: c.affinity,
                }
            } else {
                ed.cursor_at_byte(i, c.affinity)
            }
        };
        let anchor = clamp(self, self.selection.anchor);
        let focus = clamp(self, self.selection.focus);
        self.selection = Selection::new(anchor, focus);
    }

    fn len(&self) -> u32 {
        self.buffer.len() as u32
    }

    /// Heap bytes held (capacity): the buffer, the layout, and the
    /// derived cluster and word tables.
    pub fn heap_bytes(&self) -> usize {
        self.buffer.capacity()
            + self.layout.heap_bytes()
            + self.clusters.capacity() * std::mem::size_of::<Range<u32>>()
            + self.words.capacity() * std::mem::size_of::<u32>()
    }

    // --- MARK: clusters and cursors ---

    /// The logical cluster holding byte `index`.
    fn cluster(&self, index: u32) -> Option<Range<u32>> {
        let k = self.clusters.partition_point(|c| c.end <= index);
        self.clusters.get(k).filter(|c| c.contains(&index)).cloned()
    }

    fn cluster_is_newline(&self, c: &Range<u32>) -> bool {
        self.buffer[c.start as usize..c.end as usize]
            .chars()
            .any(is_newline)
    }

    fn is_word_boundary(&self, c: &Range<u32>) -> bool {
        self.words.binary_search(&c.start).is_ok()
    }

    fn is_space(&self, c: &Range<u32>) -> bool {
        let s = &self.buffer[c.start as usize..c.end as usize];
        !s.is_empty() && s.chars().all(char::is_whitespace)
    }

    /// A cursor at the cluster boundary at or before `index`.
    fn cursor_at_byte(&self, index: u32, affinity: Affinity) -> Cursor {
        match self.cluster(index) {
            Some(c) => Cursor {
                index: c.start,
                affinity: if c.start == 0 {
                    Affinity::Downstream
                } else {
                    affinity
                },
            },
            None => Cursor {
                index: self.len(),
                affinity: Affinity::Upstream,
            },
        }
    }

    /// A cursor for a selection end at `index` (Parley's `cursor_at`).
    fn cursor_at(&self, index: u32) -> Cursor {
        if index >= self.len() {
            self.cursor_at_byte(self.len(), Affinity::Upstream)
        } else {
            self.cursor_at_byte(index, Affinity::Downstream)
        }
    }

    fn affinity_for(rtl: bool, moving_right: bool) -> Affinity {
        if rtl == moving_right {
            Affinity::Downstream
        } else {
            Affinity::Upstream
        }
    }

    /// The line a cursor draws on, and the clusters left and right of it
    /// in visual order. No allocation.
    fn slot(&self, c: Cursor) -> Slot {
        let p = &self.layout;
        // The cluster the cursor belongs to, and whether it sits at that
        // cluster's logical end. A cursor after a newline belongs to the
        // next line.
        let upstream = (c.affinity == Affinity::Upstream && c.index > 0)
            .then(|| self.cluster(c.index - 1))
            .flatten()
            .filter(|u| !self.cluster_is_newline(u));
        let owner = match upstream {
            Some(u) => Some((u, true)),
            None => self.cluster(c.index).map(|d| (d, false)).or_else(|| {
                (c.index > 0)
                    .then(|| self.cluster(c.index - 1))
                    .flatten()
                    .filter(|u| !self.cluster_is_newline(u))
                    .map(|u| (u, true))
            }),
        };
        let Some((owner, at_end)) = owner else {
            // Empty text, or after a final newline: the last line.
            let line = p.lines.len().saturating_sub(1);
            return Slot {
                line,
                left: None,
                right: None,
            };
        };
        let line = p.line_of(owner.start);
        let mut prev = None;
        let mut it = p.line_clusters(line);
        while let Some(v) = it.next() {
            if v.text == owner {
                // Left edge: logical start of an LTR cluster, end of an RTL
                // one.
                return if at_end != v.rtl {
                    Slot {
                        line,
                        right: it.next(),
                        left: Some(v),
                    }
                } else {
                    Slot {
                        line,
                        left: prev,
                        right: Some(v),
                    }
                };
            }
            prev = Some(v);
        }
        Slot {
            line,
            left: None,
            right: None,
        }
    }

    /// The caret x and line of a cursor.
    fn caret_x(&self, c: Cursor) -> (usize, f32) {
        let s = self.slot(c);
        let x = match (&s.left, &s.right) {
            (_, Some(r)) => r.left,
            (Some(l), None) => l.right,
            (None, None) => self.layout.caret(self.len()).x,
        };
        (s.line, x)
    }

    /// The cursor at the left edge of a cluster starting a line.
    fn left_edge(&self, v: &VisualCluster) -> Cursor {
        if v.rtl {
            self.cursor_at_byte(v.text.end, Affinity::Upstream)
        } else {
            self.cursor_at_byte(v.text.start, Affinity::Downstream)
        }
    }

    /// The cursor at the right edge of a cluster ending a line (before it,
    /// for a newline).
    fn right_edge(&self, v: &VisualCluster) -> Cursor {
        if self.cluster_is_newline(&v.text) || v.rtl {
            self.cursor_at_byte(v.text.start, Affinity::Downstream)
        } else {
            self.cursor_at_byte(v.text.end, Affinity::Upstream)
        }
    }

    fn next_visual(&self, c: Cursor) -> Cursor {
        let s = self.slot(c);
        if let Some(v) = &s.right {
            if self.cluster_is_newline(&v.text) {
                // Past a hard break: the next line's start.
                return self.cursor_at_byte(v.text.end, Affinity::Downstream);
            }
            let index = if v.rtl { v.text.start } else { v.text.end };
            return self.cursor_at_byte(index, Self::affinity_for(v.rtl, true));
        }
        match self.layout.line_clusters(s.line + 1).next() {
            Some(v) => self.left_edge(&v),
            None => c,
        }
    }

    fn previous_visual(&self, c: Cursor) -> Cursor {
        let s = self.slot(c);
        if let Some(v) = &s.left {
            let index = if v.rtl { v.text.end } else { v.text.start };
            return self.cursor_at_byte(index, Self::affinity_for(v.rtl, false));
        }
        let prev = s
            .line
            .checked_sub(1)
            .and_then(|l| self.layout.line_clusters(l).last());
        match prev {
            Some(v) => self.right_edge(&v),
            None => c,
        }
    }

    /// The clusters left and right of a cursor, in visual order.
    /// Across a line boundary, the neighbour is the other line's
    /// nearest cluster (word moves continue through soft and hard breaks,
    /// as Parley's cluster navigation does).
    fn visual_neighbours(&self, c: Cursor) -> (Option<VisualCluster>, Option<VisualCluster>) {
        let s = self.slot(c);
        let left = s.left.or_else(|| {
            s.line
                .checked_sub(1)
                .and_then(|l| self.layout.line_clusters(l).last())
        });
        let right = s
            .right
            .or_else(|| self.layout.line_clusters(s.line + 1).next());
        (left, right)
    }

    /// Right cluster by cluster to the end of a word: where a word starts
    /// after a non-space (the left cluster's side in right-to-left text).
    fn next_visual_word(&self, c: Cursor) -> Cursor {
        let mut cur = c;
        loop {
            let next = self.next_visual(cur);
            if next == cur {
                break;
            }
            cur = next;
            let (Some(left), Some(right)) = self.visual_neighbours(cur) else {
                break;
            };
            let stop = if left.rtl {
                self.is_word_boundary(&left.text) && !self.is_space(&left.text)
            } else {
                self.is_word_boundary(&right.text) && !self.is_space(&left.text)
            };
            if stop {
                break;
            }
        }
        cur
    }

    /// Left cluster by cluster to the start of a word.
    fn previous_visual_word(&self, c: Cursor) -> Cursor {
        let mut cur = c;
        loop {
            let next = self.previous_visual(cur);
            if next == cur {
                break;
            }
            cur = next;
            let (Some(left), Some(right)) = self.visual_neighbours(cur) else {
                break;
            };
            let stop = if left.rtl {
                self.is_word_boundary(&left.text)
                    && (self.is_space(&left.text)
                        || (self.is_word_boundary(&right.text) && !self.is_space(&right.text)))
            } else {
                self.is_word_boundary(&right.text) && !self.is_space(&right.text)
            };
            if stop {
                break;
            }
        }
        cur
    }

    /// A range selection's visually last (right: greater line, then
    /// greater x) or first end.
    fn visual_end(&self, right: bool) -> Cursor {
        let (a, f) = (self.selection.anchor, self.selection.focus);
        let key = |c: Cursor| {
            let (line, x) = self.caret_x(c);
            (line, x)
        };
        let (ka, kf) = (key(a), key(f));
        let a_after = ka.0 > kf.0 || (ka.0 == kf.0 && ka.1 > kf.1);
        let a_before = ka.0 < kf.0 || (ka.0 == kf.0 && ka.1 < kf.1);
        if (right && a_after) || (!right && a_before) {
            a
        } else {
            f
        }
    }

    /// The next word boundary after a cursor, in logical order (text end
    /// if none).
    fn next_logical_word(&self, c: Cursor) -> Cursor {
        let from = self
            .cluster(c.index)
            .or_else(|| c.index.checked_sub(1).and_then(|i| self.cluster(i)));
        let Some(from) = from else {
            return c;
        };
        let k = self.clusters.partition_point(|x| x.start <= from.start);
        match self.clusters[k..].iter().find(|x| self.is_word_boundary(x)) {
            Some(x) => self.cursor_at_byte(x.start, Affinity::Downstream),
            None => self.cursor_at_byte(self.len(), Affinity::Upstream),
        }
    }

    /// The previous word boundary before a cursor, in logical order.
    fn previous_logical_word(&self, c: Cursor) -> Cursor {
        let from = c
            .index
            .checked_sub(1)
            .and_then(|i| self.cluster(i))
            .or_else(|| self.cluster(c.index));
        let Some(from) = from else {
            return c;
        };
        let k = self.clusters.partition_point(|x| x.start < from.start);
        let target = self.clusters[..k]
            .iter()
            .rev()
            .find(|x| self.is_word_boundary(x))
            .unwrap_or(&from);
        self.cursor_at_byte(target.start, Affinity::Downstream)
    }

    /// The cursor nearest a point (paragraph space).
    fn cursor_from_point(&self, x: f32, y: f32) -> Cursor {
        let hit = self.layout.hit(x, y);
        Cursor {
            index: hit.offset,
            affinity: if hit.downstream {
                Affinity::Downstream
            } else {
                Affinity::Upstream
            },
        }
    }

    /// The cluster under a point, or the nearest on its line.
    fn cluster_at_point(&self, x: f32, y: f32) -> Option<VisualCluster> {
        let p = &self.layout;
        let line = p
            .lines
            .iter()
            .position(|l| y < l.top + l.height)
            .or(p.lines.len().checked_sub(1))?;
        let vc = p.line_clusters(line);
        let dist = |v: &VisualCluster| {
            if x < v.left {
                v.left - x
            } else if x >= v.right {
                x - v.right
            } else {
                -1.0
            }
        };
        vc.into_iter().min_by(|a, b| dist(a).total_cmp(&dist(b)))
    }

    fn word_selection_at(&self, x: f32, y: f32) -> Selection {
        let Some(v) = self.cluster_at_point(x, y) else {
            return Selection::collapsed(self.cursor_at_byte(self.len(), Affinity::Upstream));
        };
        let mut cluster = v.text.clone();
        if !self.is_word_boundary(&cluster) {
            let k = self.clusters.partition_point(|x| x.start < cluster.start);
            if let Some(prev) = self.clusters[..k]
                .iter()
                .rev()
                .find(|x| self.is_word_boundary(x))
            {
                cluster = prev.clone();
            }
        }
        let rtl = self
            .layout
            .line_clusters(self.layout.line_of(cluster.start))
            .find(|x| x.text == cluster)
            .is_some_and(|x| x.rtl);
        let anchor = self.cursor_at_byte(cluster.start, Self::affinity_for(rtl, !rtl));
        let focus = self.next_logical_word(anchor);
        Selection {
            anchor,
            focus,
            h_pos: None,
            anchor_base: AnchorBase::Word(anchor, focus),
        }
    }

    fn line_start(&self, c: Cursor) -> Cursor {
        let line = self.slot(c).line;
        match self.layout.lines.get(line) {
            Some(l) => self.cursor_at_byte(l.text.start, Affinity::Downstream),
            None => c,
        }
    }

    fn line_end(&self, c: Cursor) -> Cursor {
        let line = self.slot(c).line;
        let Some(l) = self.layout.lines.get(line) else {
            return c;
        };
        if l.caret_end < l.text.end {
            // A hard break: before the newline.
            self.cursor_at_byte(l.caret_end, Affinity::Downstream)
        } else {
            self.cursor_at_byte(l.text.end, Affinity::Upstream)
        }
    }

    /// The focus moved `delta` lines, keeping the horizontal goal.
    fn move_lines(&self, delta: isize, extend: bool) -> Selection {
        let sel = self.selection;
        let last = self.layout.lines.len().saturating_sub(1);
        let (line, x) = self.caret_x(sel.focus);
        let h_pos = sel.h_pos.unwrap_or(x);
        let target = line as isize + delta;
        let to_line = |i: usize| {
            let l = &self.layout.lines[i];
            self.cursor_from_point(h_pos, l.top + l.height * 0.5)
        };
        let focus = if target < 0 {
            self.line_start(to_line(0))
        } else if target as usize > last {
            self.line_end(to_line(last))
        } else {
            to_line(target as usize)
        };
        let mut next = if extend {
            Selection::new(sel.anchor, focus)
        } else {
            Selection::collapsed(focus)
        };
        if (0..=last as isize).contains(&target) {
            next.h_pos = Some(h_pos);
        }
        next
    }

    fn set_selection(&mut self, next: Selection) {
        if next.focus != self.selection.focus || next.anchor != self.selection.anchor {
            self.generation += 1;
        }
        self.selection = next;
    }

    fn move_focus(&mut self, focus: Cursor, extend: bool) {
        let next = if extend {
            Selection::new(self.selection.anchor, focus)
        } else {
            Selection::collapsed(focus)
        };
        self.set_selection(next);
    }

    // --- MARK: motion (the layout must be clean) ---

    /// Applies a motion; `extend` moves the focus only.
    pub fn motion(&mut self, m: Motion, extend: bool) {
        debug_assert!(self.is_clean());
        let focus = self.selection.focus;
        let range = !self.selection.is_collapsed() && !extend;
        match m {
            // With a range selected, Left and Right collapse it to its
            // visual end.
            Motion::Left if range => self.move_focus(self.visual_end(false), false),
            Motion::Right if range => self.move_focus(self.visual_end(true), false),
            Motion::Left => self.move_focus(self.previous_visual(focus), extend),
            Motion::Right => self.move_focus(self.next_visual(focus), extend),
            Motion::WordLeft => self.move_focus(self.previous_visual_word(focus), extend),
            Motion::WordRight => self.move_focus(self.next_visual_word(focus), extend),
            Motion::LineStart => self.move_focus(self.line_start(focus), extend),
            Motion::LineEnd => self.move_focus(self.line_end(focus), extend),
            Motion::Up => {
                let s = self.move_lines(-1, extend);
                self.set_selection(s);
            }
            Motion::Down => {
                let s = self.move_lines(1, extend);
                self.set_selection(s);
            }
            Motion::TextStart => {
                let s = self.move_lines(isize::MIN / 2, extend);
                self.set_selection(s);
            }
            Motion::TextEnd => {
                let s = self.move_lines(isize::MAX / 2, extend);
                self.set_selection(s);
            }
        }
    }

    pub fn select_all(&mut self) {
        let start = self.cursor_at_byte(0, Affinity::Downstream);
        self.selection = Selection::collapsed(start);
        let s = self.move_lines(isize::MAX / 2, true);
        self.set_selection(Selection::new(start, s.focus));
    }

    pub fn move_to_point(&mut self, x: f32, y: f32) {
        let c = self.cursor_from_point(x, y);
        self.set_selection(Selection::collapsed(c));
    }

    /// Extends the selection to a point, by words when it began as a word.
    pub fn extend_to_point(&mut self, x: f32, y: f32) {
        let next = match self.selection.anchor_base {
            AnchorBase::Cluster => {
                Selection::new(self.selection.anchor, self.cursor_from_point(x, y))
            }
            AnchorBase::Word(start, end) => {
                let target = self.word_selection_at(x, y);
                let (anchor, focus) = if target.anchor.index < start.index {
                    (end, target.anchor)
                } else {
                    (start, target.focus)
                };
                Selection {
                    anchor,
                    focus,
                    h_pos: None,
                    anchor_base: self.selection.anchor_base,
                }
            }
        };
        self.set_selection(next);
    }

    pub fn select_word_at_point(&mut self, x: f32, y: f32) {
        let s = self.word_selection_at(x, y);
        self.set_selection(s);
    }

    /// Restores a selection of full cursors (undo), each moved onto a
    /// valid cluster boundary of the current layout.
    pub fn set_selection_cursors(&mut self, anchor: Cursor, focus: Cursor) {
        self.selection = Selection::new(anchor, focus);
        self.clamp_selection();
        self.generation += 1;
    }

    /// Selects bytes `anchor..focus` (no-op off char boundaries).
    pub fn select_byte_range(&mut self, anchor: u32, focus: u32) {
        let ok = |i: u32| self.buffer.is_char_boundary(i as usize);
        if ok(anchor) && ok(focus) {
            let s = Selection::new(self.cursor_at(anchor), self.cursor_at(focus));
            self.set_selection(s);
        }
    }

    // --- MARK: edits (each shapes once) ---

    fn replace(&mut self, engine: &mut TextEngine, range: Range<u32>, s: &str) {
        self.buffer
            .replace_range(range.start as usize..range.end as usize, s);
        self.update_compose(range, s.len() as u32);
        self.shape_dirty = true;
        self.generation += 1;
        self.refresh(engine);
    }

    /// Keeps the preedit range on the buffer after bytes `old` became
    /// `new_len` bytes: an edit before it shifts it, an edit after it
    /// leaves it, and an edit that overlaps it ends composing (its text
    /// stays as committed text). A shifted range keeps its character
    /// boundaries.
    fn update_compose(&mut self, old: Range<u32>, new_len: u32) {
        let Some(c) = self.compose.clone() else {
            return;
        };
        let removed = old.end - old.start;
        if old.end <= c.start {
            self.compose = Some(c.start - removed + new_len..c.end - removed + new_len);
        } else if old.start < c.end {
            self.compose = None;
            self.show_cursor = true;
        }
    }

    /// Inserts at the caret or replaces the selection.
    pub fn insert_or_replace_selection(&mut self, engine: &mut TextEngine, s: &str) {
        let range = self.selection.text_range();
        let start = range.start;
        self.replace(engine, range, s);
        let index = start + s.len() as u32;
        let affinity = if s.ends_with(['\n', '\r', '\u{2028}', '\u{2029}']) {
            Affinity::Downstream
        } else {
            Affinity::Upstream
        };
        let c = self.cursor_at_byte(index, affinity);
        self.set_selection(Selection::collapsed(c));
    }

    pub fn delete_selection(&mut self, engine: &mut TextEngine) {
        self.insert_or_replace_selection(engine, "");
    }

    /// Deletes the selection or the next cluster.
    pub fn delete(&mut self, engine: &mut TextEngine) {
        if !self.selection.is_collapsed() {
            return self.delete_selection(engine);
        }
        if let Some(c) = self.cluster(self.selection.focus.index) {
            self.replace(engine, c, "");
        }
    }

    /// Deletes the selection, or backwards: a whole newline or emoji
    /// cluster, else one character.
    pub fn backdelete(&mut self, engine: &mut TextEngine) {
        if !self.selection.is_collapsed() {
            return self.delete_selection(engine);
        }
        let end = self.selection.focus.index;
        let Some(c) = end.checked_sub(1).and_then(|i| self.cluster(i)) else {
            return;
        };
        let text = &self.buffer[c.start as usize..end as usize];
        let start = if self.cluster_is_newline(&c) || emoji_presentation(text) {
            c.start
        } else {
            let Some((i, _)) = self.buffer[..end as usize].char_indices().next_back() else {
                return;
            };
            i as u32
        };
        self.replace(engine, start..end, "");
        let c = self.cursor_at_byte(start, Affinity::Downstream);
        self.set_selection(Selection::collapsed(c));
    }

    /// Deletes the selection or up to the next word boundary.
    pub fn delete_word(&mut self, engine: &mut TextEngine) {
        if !self.selection.is_collapsed() {
            return self.delete_selection(engine);
        }
        let start = self.selection.focus.index;
        let end = self.next_logical_word(self.selection.focus).index;
        if end > start {
            self.replace(engine, start..end, "");
            let c = self.cursor_at_byte(start, Affinity::Downstream);
            self.set_selection(Selection::collapsed(c));
        }
    }

    /// Deletes the selection or back to the previous word boundary.
    pub fn backdelete_word(&mut self, engine: &mut TextEngine) {
        if !self.selection.is_collapsed() {
            return self.delete_selection(engine);
        }
        let end = self.selection.focus.index;
        let start = self.previous_logical_word(self.selection.focus).index;
        if start < end {
            self.replace(engine, start..end, "");
            let c = self.cursor_at_byte(start, Affinity::Downstream);
            self.set_selection(Selection::collapsed(c));
        }
    }

    // --- MARK: IME ---

    /// Sets the preedit (replacing the selection when composing starts);
    /// `cursor` is a byte range within `text`, None hides the caret.
    pub fn set_compose(&mut self, engine: &mut TextEngine, text: &str, cursor: Option<(u32, u32)>) {
        let start = match self.compose.clone() {
            Some(c) => {
                self.buffer
                    .replace_range(c.start as usize..c.end as usize, text);
                c.start
            }
            None => {
                let r = self.selection.text_range();
                self.buffer
                    .replace_range(r.start as usize..r.end as usize, text);
                r.start
            }
        };
        self.compose = Some(start..start + text.len() as u32);
        self.show_cursor = cursor.is_some();
        self.shape_dirty = true;
        self.generation += 1;
        self.refresh(engine);
        let (a, b) = cursor.unwrap_or((0, 0));
        let s = Selection::new(self.cursor_at(start + a), self.cursor_at(start + b));
        self.set_selection(s);
    }

    /// Removes the preedit and puts the caret where it began.
    pub fn clear_compose(&mut self, engine: &mut TextEngine) {
        if let Some(c) = self.compose.take() {
            self.buffer
                .replace_range(c.start as usize..c.end as usize, "");
            self.show_cursor = true;
            self.shape_dirty = true;
            self.generation += 1;
            self.refresh(engine);
            let cur = self.cursor_at(c.start);
            self.set_selection(Selection::collapsed(cur));
        }
    }

    /// Keeps the preedit as committed text. The composing underline goes,
    /// so the layout shapes once.
    pub fn finish_compose(&mut self, engine: &mut TextEngine) {
        if self.compose.take().is_some() {
            self.show_cursor = true;
            self.shape_dirty = true;
            self.generation += 1;
            self.refresh(engine);
        }
    }

    // --- MARK: geometry (the layout must be clean) ---

    /// The caret rectangle, `width` wide; None when the IME hides it.
    pub fn caret_rect(&self, width: f32) -> Option<Rect> {
        if !self.show_cursor {
            return None;
        }
        let (line, x) = self.caret_x(self.selection.focus);
        let (top, height) = match self.layout.lines.get(line) {
            Some(l) => (l.top, l.height),
            None => {
                let c = self.layout.caret(0);
                (c.top, c.height)
            }
        };
        Some((x, top, width, height))
    }

    /// Rectangles covering the selection; a selected hard break shows as
    /// a quarter of the line's ascent + descent past the line's end.
    pub fn selection_rects(&self) -> Vec<Rect> {
        if self.selection.is_collapsed() {
            return Vec::new();
        }
        let range = self.selection.text_range();
        let mut out = self.layout.selection_rects(range.clone());
        for line in &self.layout.lines {
            let nl = line.caret_end..line.text.end;
            if line.caret_end < line.text.end && range.start <= nl.start && nl.end <= range.end {
                let w = (line.ascent + line.descent) * 0.25;
                let x = self
                    .layout
                    .clusters_of(line.segs.clone())
                    .map(|v| v.right)
                    .fold(line.x, f32::max);
                match out.iter_mut().find(|r| r.1 == line.top && r.0 + r.2 == x) {
                    Some(r) => r.2 += w,
                    None => out.push((x, line.top, w, line.height)),
                }
            }
        }
        out
    }

    /// The area the IME candidate window should avoid: the preedit, else
    /// the caret with the selection on its line, widened by three average
    /// advances each side and clamped to the wrap width.
    pub fn ime_area(&self) -> Rect {
        let union = |a: Rect, b: Rect| {
            let x0 = a.0.min(b.0);
            let y0 = a.1.min(b.1);
            let x1 = (a.0 + a.2).max(b.0 + b.2);
            let y1 = (a.1 + a.3).max(b.1 + b.3);
            (x0, y0, x1 - x0, y1 - y0)
        };
        let caret = |c: Cursor| {
            let (line, x) = self.caret_x(c);
            let l = self.layout.lines.get(line);
            (
                x,
                l.map_or(0.0, |l| l.top),
                0.0,
                l.map_or(self.size, |l| l.height),
            )
        };
        let area = match &self.compose {
            Some(c) => self
                .layout
                .selection_rects(c.clone())
                .into_iter()
                .reduce(union)
                .unwrap_or_else(|| caret(self.cursor_at(c.start))),
            None => {
                let focus = caret(self.selection.focus);
                self.selection_rects()
                    .into_iter()
                    .filter(|r| r.1 == focus.1)
                    .fold(focus, union)
            }
        };
        let inflate = 3.0 * 0.6 * self.size;
        let x0 = (area.0 - inflate).max(0.0);
        let x1 = (area.0 + area.2 + inflate).min(self.width.unwrap_or(f32::INFINITY));
        (x0, area.1, (x1 - x0).max(0.0), area.3)
    }
}

/// Where a cursor sits: its line and the clusters beside it.
struct Slot {
    line: usize,
    left: Option<VisualCluster>,
    right: Option<VisualCluster>,
}

/// A caret motion (`Editor::motion`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    Up,
    Down,
    TextStart,
    TextEnd,
}
