//! The owned paragraph (ARCHITECTURE.md §5).
//!
//! ```text
//! UTF-8 + spans -> Unicode analysis -> font resolution + fallback
//!   -> shaping (HarfRust) -> runs/clusters -> lines + placements
//! ```
//!
//! A `Paragraph` keeps its runs (text range, font instance, size, bidi
//! level, metrics), its glyphs, and its lines. The glyphs are the one
//! placement store: hit testing, carets, selection geometry, and chunk
//! emission all read their positions. Colors are not here: a glyph
//! carries its span index, which is its paint slot, so a color change
//! touches no placement.
//!
//! Shaping and line breaking are separate: `rewrap` lays out an already
//! shaped paragraph at another width without shaping again. What line
//! breaking needs from Unicode analysis is kept as one flag byte per
//! text byte (`Paragraph::analysis`): UAX #14 break opportunities, the
//! bidi paragraph direction, and the UAX #9 L1 classes. Bidi levels live
//! on the runs. Graphemes and scripts are shaping scratch.
//!
//! Reordering follows UAX #9 L1 and L2 per line, over the line's
//! clusters: trailing whitespace (and whitespace before a tab or
//! paragraph separator) takes the paragraph level. Parley does not apply
//! L1 at soft line ends; this is the one known difference from the
//! oracle.
//!
//! Line breaking follows Parley's default semantics (the E01 oracle):
//! break opportunities from UAX #14, not at a line's start; overflowing
//! spaces hang; a newline ends a line; content with no opportunity
//! overflows. Line metrics too: a line is as tall as its tallest run's
//! ascent + descent + leading; ascent and descent come from runs that
//! are not trailing whitespace; the leading splits evenly.

use std::collections::HashMap;
use std::ops::Range;

use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};
use skrifa::raw::types::F2Dot14;
use unicode_bidi::{BidiClass, BidiInfo};
use unicode_segmentation::UnicodeSegmentation;

use crate::fonts::{FontAttrs, FontInstanceId, FontStore, ScriptTag, ignorable};

/// Style of a span: what shaping and metrics read. Color and
/// decorations are not here: they are paint (`TextEngine::emit_paragraph`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
    /// The span's primary font, resolved from its family once when the
    /// span is applied (`TextEngine::font`). None: the engine's default
    /// family (resolved at layout through the same cache).
    pub font: Option<FontInstanceId>,
    /// Added to each cluster's advance, logical points.
    pub letter_spacing: f32,
    /// Absolute line height, logical points; 0: the font's. Read from
    /// span zero only (per paragraph).
    pub line_height: f32,
}

impl Default for TextStyle {
    fn default() -> TextStyle {
        TextStyle {
            size: 14.0,
            weight: 400,
            italic: false,
            font: None,
            letter_spacing: 0.0,
            line_height: 0.0,
        }
    }
}

/// A styled range: from byte `start` to the next span's start. Span zero
/// starts at 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpanStyle {
    pub start: u32,
    pub style: TextStyle,
}

/// What to lay out. Span zero is the base style; there is no paragraph
/// family.
#[derive(Clone, Copy)]
pub struct TextSpec<'a> {
    pub text: &'a str,
    pub spans: &'a [SpanStyle],
}

/// Vertical metrics of a run, logical points (descent positive), with
/// its decoration geometry (offsets are distances above the baseline to
/// the decoration's top; negative below).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RunMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
    /// The run's line height: the absolute value of span zero, else
    /// ascent + descent + leading.
    pub line_height: f32,
    pub underline_offset: f32,
    pub underline_size: f32,
    pub strike_offset: f32,
    pub strike_size: f32,
}

/// A shaped run: one font instance, size, bidi level, and script over a
/// text range. Glyphs are in visual order within the run.
#[derive(Clone, Debug)]
pub struct Run {
    pub text: Range<u32>,
    pub font: FontInstanceId,
    pub size: f32,
    pub level: u8,
    pub glyphs: Range<u32>,
    pub metrics: RunMetrics,
}

impl Run {
    pub fn rtl(&self) -> bool {
        self.level & 1 == 1
    }
}

/// A glyph id marking a glyph that is not drawn (a newline).
pub const HIDDEN: u16 = u16::MAX;

/// One placed glyph: the placement store's row (28 bytes). Every
/// position consumer reads it: drawing at (x + dx, y), and hit testing,
/// carets, and selection at cluster edges (the x of a cluster's first
/// placed glyph, and the pen after its last).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub id: u16,
    /// Span index: the paint slot.
    pub style: u16,
    /// Byte offset of the glyph's cluster start.
    pub cluster: u32,
    pub advance: f32,
    /// Shaping offsets (y down).
    pub dx: f32,
    pub dy: f32,
    /// Pen x (the left edge of the glyph's advance) and baseline + dy,
    /// paragraph space. The glyph draws at x + dx.
    pub x: f32,
    pub y: f32,
}

/// A run's slice on a line, in visual order.
#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub run: u32,
    pub glyphs: Range<u32>,
    pub text: Range<u32>,
    /// Bidi level after L1: the run's level, or the paragraph's for
    /// trailing whitespace. When its direction differs from the run's,
    /// the segment's clusters are placed in the reverse of their stored
    /// order (glyphs inside a cluster keep theirs).
    pub level: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: Range<u32>,
    /// Visual-order segments.
    pub segs: Range<u32>,
    pub top: f32,
    pub height: f32,
    pub baseline: f32,
    pub ascent: f32,
    pub descent: f32,
    /// Content advance including trailing whitespace, and the trailing
    /// whitespace alone.
    pub advance: f32,
    pub trailing: f32,
    /// The last caret offset on the line: before a line-ending newline,
    /// else the line's end.
    pub caret_end: u32,
    /// Left edge of the line's content (alignment applied).
    pub x: f32,
}

/// A laid-out paragraph.
#[derive(Clone, Debug, Default)]
pub struct Paragraph {
    pub runs: Vec<Run>,
    pub glyphs: Vec<Glyph>,
    pub lines: Vec<Line>,
    pub segs: Vec<Seg>,
    pub width: f32,
    pub height: f32,
    /// Metrics of the first span's font: an empty paragraph's caret.
    pub empty_metrics: RunMetrics,
    pub text_len: u32,
    pub base_rtl: bool,
    /// The wrap width the lines were made for (None: unbounded).
    pub max_width: Option<f32>,
    /// One byte of analysis flags per text byte: `BREAK` and `PARA_RTL`
    /// per byte, `L1_WS` and `L1_SEP` on a character's first byte, and
    /// the cluster flags (`CL_*`) on a cluster's first byte.
    pub analysis: Vec<u8>,
}

/// UAX #14 allows a line break before this byte.
pub const BREAK: u8 = 1;
/// The byte's bidi paragraph is right-to-left.
pub const PARA_RTL: u8 = 2;
/// L1 whitespace: bidi class WS, an isolate or embedding control, or BN.
pub const L1_WS: u8 = 4;
/// L1 separator: bidi class S (tab) or B (paragraph separator).
pub const L1_SEP: u8 = 8;
/// The cluster holds a newline.
pub const CL_NEWLINE: u8 = 16;
/// The cluster is whitespace only (and no newline).
pub const CL_SPACE: u8 = 32;
/// Every character of the cluster is L1 whitespace.
pub const CL_L1_WS: u8 = 64;
/// A character of the cluster is an L1 separator.
pub const CL_L1_SEP: u8 = 128;

/// Rewrap scratch kept across paragraphs (the engine holds one).
#[derive(Default)]
pub struct WrapScratch {
    clusters: Vec<Cluster>,
}

/// The clusters of a run of segments in visual order
/// (`Paragraph::clusters_of`).
pub struct LineClusters<'a> {
    p: &'a Paragraph,
    /// Segments not started yet.
    segs: Range<u32>,
    /// The current segment: its glyphs, whether it places its clusters
    /// reversed, its direction, its text end, and the group cursor.
    seg: Option<(Range<u32>, bool, bool, u32, u32)>,
    /// The next group of the current segment (one of lookahead).
    pending: Option<Range<u32>>,
    /// The cluster byte of the group before it.
    prev: Option<u32>,
}

impl Iterator for LineClusters<'_> {
    type Item = VisualCluster;

    fn next(&mut self) -> Option<VisualCluster> {
        let p = self.p;
        loop {
            if let Some((glyphs, reversed, ltr, text_end, cursor)) = &mut self.seg
                && let Some(g) = self.pending.take()
            {
                let next = next_group(&p.glyphs, glyphs, *reversed, cursor);
                let first = |g: &Range<u32>| &p.glyphs[g.start as usize];
                let left = first(&g).x;
                let right = match &next {
                    Some(n) => first(n).x,
                    None => {
                        let last = &p.glyphs[g.end as usize - 1];
                        last.x + last.advance
                    }
                };
                // Left to right, cluster starts rise at an even level and
                // fall at an odd one: a cluster ends where the next in
                // logical order starts.
                let end = if *ltr {
                    next.as_ref().map(|n| first(n).cluster)
                } else {
                    self.prev
                };
                let start = first(&g).cluster;
                self.prev = Some(start);
                self.pending = next;
                return Some(VisualCluster {
                    text: start..end.unwrap_or(*text_end),
                    rtl: !*ltr,
                    left,
                    right,
                });
            }
            // The next segment.
            if self.segs.start >= self.segs.end {
                return None;
            }
            let seg = &p.segs[self.segs.start as usize];
            self.segs.start += 1;
            let reversed = p.reversed(seg);
            let mut cursor = group_cursor(&seg.glyphs, reversed);
            self.pending = next_group(&p.glyphs, &seg.glyphs, reversed, &mut cursor);
            self.prev = None;
            self.seg = Some((
                seg.glyphs.clone(),
                reversed,
                seg.level & 1 == 0,
                seg.text.end,
                cursor,
            ));
        }
    }
}

/// One cluster of a line in visual order (`Paragraph::visual_clusters`).
#[derive(Clone, Debug, PartialEq)]
pub struct VisualCluster {
    pub text: Range<u32>,
    /// Laid out right to left (odd level after L1).
    pub rtl: bool,
    pub left: f32,
    pub right: f32,
}

/// A caret: a vertical bar at `x` from `top` for `height`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub x: f32,
    pub top: f32,
    pub height: f32,
    pub line: u32,
}

/// Where a point falls: a byte offset (a cluster boundary) and whether
/// the caret belongs to the text after it (downstream) or before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub offset: u32,
    pub downstream: bool,
}

/// One cluster in logical order (scratch for line breaking).
#[derive(Clone, Debug)]
struct Cluster {
    text: Range<u32>,
    run: u32,
    glyphs: Range<u32>,
    advance: f32,
    space: bool,
    newline: bool,
    /// UAX #14 allows a break before this cluster.
    boundary: bool,
    /// Every character is L1 whitespace.
    l1_ws: bool,
    /// A character is an L1 separator.
    l1_sep: bool,
    /// L1 sets this cluster to the paragraph level (per line).
    reset: bool,
}

pub(crate) fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{2028}' | '\u{2029}' | '\u{0B}' | '\u{0C}' | '\u{85}'
    )
}

/// The start cursor of `next_group` over a segment's glyphs.
fn group_cursor(glyphs: &Range<u32>, reversed: bool) -> u32 {
    if reversed { glyphs.end } else { glyphs.start }
}

/// The next cluster of a segment in placement order, as a glyph range:
/// stored order, or clusters from the end when `reversed`. Glyphs inside
/// a cluster keep their stored order either way.
fn next_group(
    glyphs: &[Glyph],
    seg: &Range<u32>,
    reversed: bool,
    cursor: &mut u32,
) -> Option<Range<u32>> {
    let cluster = |i: u32| glyphs[i as usize].cluster;
    if reversed {
        if *cursor <= seg.start {
            return None;
        }
        let end = *cursor;
        let c = cluster(end - 1);
        let mut start = end - 1;
        while start > seg.start && cluster(start - 1) == c {
            start -= 1;
        }
        *cursor = start;
        Some(start..end)
    } else {
        if *cursor >= seg.end {
            return None;
        }
        let start = *cursor;
        let c = cluster(start);
        let mut end = start + 1;
        while end < seg.end && cluster(end) == c {
            end += 1;
        }
        *cursor = end;
        Some(start..end)
    }
}

/// Whether `font` covers every character of `cluster` that needs a
/// glyph.
pub(crate) fn covers_cluster(store: &mut FontStore, font: FontInstanceId, cluster: &str) -> bool {
    cluster
        .chars()
        .filter(|&c| !ignorable(c))
        .all(|c| store.covers(font, c))
}

impl Paragraph {
    fn span_at(spans: &[SpanStyle], byte: u32) -> usize {
        spans.partition_point(|s| s.start <= byte).saturating_sub(1)
    }

    /// Clusters in logical order: glyphs grouped by cluster value within
    /// each run (a right-to-left run's glyphs are reversed).
    fn clusters(&self) -> Vec<Cluster> {
        let mut out = Vec::new();
        self.clusters_into(&mut out);
        out
    }

    fn clusters_into(&self, out: &mut Vec<Cluster>) {
        out.clear();
        for (r, run) in self.runs.iter().enumerate() {
            let first = out.len();
            let g = &self.glyphs[run.glyphs.start as usize..run.glyphs.end as usize];
            let mut i = 0;
            while i < g.len() {
                let c = g[i].cluster;
                let mut j = i;
                let mut advance = 0.0;
                while j < g.len() && g[j].cluster == c {
                    advance += g[j].advance;
                    j += 1;
                }
                let glyphs = run.glyphs.start + i as u32..run.glyphs.start + j as u32;
                out.push(Cluster {
                    text: c..c,
                    run: r as u32,
                    glyphs,
                    advance,
                    space: false,
                    newline: false,
                    boundary: false,
                    l1_ws: false,
                    l1_sep: false,
                    reset: false,
                });
                i = j;
            }
            if run.rtl() {
                out[first..].reverse();
            }
            // Ends: the next cluster's start, or the run's end.
            let n = out.len();
            for k in first..n {
                out[k].text.end = if k + 1 < n {
                    out[k + 1].text.start
                } else {
                    run.text.end
                };
            }
        }
        for c in out.iter_mut() {
            let f = self
                .analysis
                .get(c.text.start as usize)
                .copied()
                .unwrap_or(0);
            c.newline = f & CL_NEWLINE != 0;
            c.space = f & CL_SPACE != 0;
            c.boundary = f & BREAK != 0;
            c.l1_ws = f & CL_L1_WS != 0;
            c.l1_sep = f & CL_L1_SEP != 0;
        }
    }

    /// Sets the cluster flags on each cluster's first byte, from the text
    /// and the per-character flags (once, after shaping).
    fn mark_clusters(&mut self, text: &str, scratch: &mut Vec<Cluster>) {
        self.clusters_into(scratch);
        for c in scratch.iter() {
            let a = c.text.start as usize;
            let s = &text[a..c.text.end as usize];
            let newline = s.chars().any(is_newline);
            let mut f = 0;
            if newline {
                f |= CL_NEWLINE;
            } else if s.chars().all(|c| {
                c.is_whitespace()
                    && unicode_linebreak::break_property(c as u32)
                        != unicode_linebreak::BreakClass::NonBreakingGlue
            }) {
                // Hanging whitespace; no-break spaces (class GL) are content.
                f |= CL_SPACE;
            }
            let chars = s.char_indices().map(|(i, _)| self.analysis[a + i]);
            if chars.clone().all(|b| b & L1_WS != 0) {
                f |= CL_L1_WS;
            }
            if chars.clone().any(|b| b & L1_SEP != 0) {
                f |= CL_L1_SEP;
            }
            self.analysis[a] |= f;
        }
    }

    /// Lays out lines at `max_width` (None: unbounded) over the shaped
    /// runs: breaks, bidi order, metrics, alignment, and final glyph
    /// positions.
    pub fn rewrap(&mut self, max_width: Option<f32>) {
        self.rewrap_with(max_width, &mut WrapScratch::default());
    }

    /// `rewrap` with reused scratch: no allocation once the scratch and
    /// the line and segment stores have grown.
    pub fn rewrap_with(&mut self, max_width: Option<f32>, scratch: &mut WrapScratch) {
        self.max_width = max_width;
        self.lines.clear();
        self.segs.clear();
        self.clusters_into(&mut scratch.clusters);
        let clusters = &mut scratch.clusters[..];
        let limit = max_width.unwrap_or(f32::MAX);
        self.base_rtl = self.analysis.first().is_some_and(|f| f & PARA_RTL != 0);

        // Line breaking over clusters (Parley's greedy rules); each line
        // is laid out as soon as its end is known.
        let mut y = 0.0f32;
        let mut width = 0.0f32;
        let mut start = 0usize;
        let mut x = 0.0f32;
        let mut opportunity: Option<usize> = None;
        let mut i = 0usize;
        let mut lines = 0usize;
        while i < clusters.len() {
            let c = &clusters[i];
            if c.boundary && x != 0.0 {
                opportunity = Some(i);
            }
            if c.newline {
                self.push_line(clusters, start..i + 1, &mut y, &mut width);
                lines += 1;
                start = i + 1;
                x = 0.0;
                opportunity = None;
                i += 1;
                continue;
            }
            let next = x + c.advance;
            if next <= limit {
                x = next;
            } else if c.space {
                // Overflowing whitespace hangs and never breaks by itself:
                // the line breaks at the next opportunity after it (the
                // next content cluster overflows there), or a hard break
                // after it closes the same line (UAX #14 LB6, LB7).
                x = next;
            } else if let Some(o) = opportunity.take() {
                self.push_line(clusters, start..o, &mut y, &mut width);
                lines += 1;
                start = o;
                x = 0.0;
                i = o;
                continue;
            } else {
                x = next;
            }
            i += 1;
        }
        // The last line; a trailing newline leaves an empty last line.
        let ends_in_newline = clusters.last().is_some_and(|c| c.newline);
        if start < clusters.len() || lines == 0 || ends_in_newline {
            let n = clusters.len();
            self.push_line(clusters, start..n, &mut y, &mut width);
        }
        // An empty last line adds no height.
        if let Some(last) = self.lines.last()
            && last.segs.is_empty()
            && self.lines.len() > 1
        {
            y -= last.height;
        }
        if self.lines.len() == 1 && self.lines[0].segs.is_empty() {
            y = 0.0;
        }
        self.width = width;
        self.height = y;

        // Alignment (start): right-to-left paragraphs align right in the
        // line box (the wrap width, else the widest line).
        let boxw = max_width.unwrap_or(width);
        for li in 0..self.lines.len() {
            let line = &self.lines[li];
            // Right-to-left: the trailing whitespace hangs to the left, and
            // the free space (negative on overflow) goes before the line.
            let x = if self.base_rtl {
                -line.trailing + (boxw - line.advance + line.trailing)
            } else {
                0.0
            };
            let (baseline, segs) = (line.baseline, line.segs.clone());
            self.lines[li].x = x;
            let mut pen = x;
            for s in segs.start..segs.end {
                let seg = &self.segs[s as usize];
                let (range, reversed) = (seg.glyphs.clone(), self.reversed(seg));
                let mut cursor = group_cursor(&range, reversed);
                while let Some(group) = next_group(&self.glyphs, &range, reversed, &mut cursor) {
                    for g in group {
                        let glyph = &mut self.glyphs[g as usize];
                        glyph.x = pen;
                        glyph.y = baseline + glyph.dy;
                        pen += glyph.advance;
                    }
                }
            }
        }
    }

    /// Lays out the line over `clusters[range]` at `*y`: metrics and
    /// visual segments (x set by alignment).
    fn push_line(
        &mut self,
        clusters: &mut [Cluster],
        range: Range<usize>,
        y: &mut f32,
        width: &mut f32,
    ) {
        let text_range = match (
            clusters[range.clone()].first(),
            clusters[range.clone()].last(),
        ) {
            (Some(a), Some(b)) => a.text.start..b.text.end,
            _ => {
                let at = clusters.last().map_or(0, |c| c.text.end);
                at..at
            }
        };
        let cl = &mut clusters[range];
        let content = cl.iter().rposition(|c| !c.space && !c.newline);
        let advance: f32 = cl.iter().map(|c| c.advance).sum();
        let trailing: f32 = match content {
            Some(k) => cl[k + 1..].iter().map(|c| c.advance).sum(),
            None => advance,
        };
        // Metrics: height from every run on the line; ascent and descent
        // from runs with content before trailing whitespace.
        let mut height = 0.0f32;
        let (mut ascent, mut descent) = (0.0f32, 0.0f32);
        for (k, c) in cl.iter().enumerate() {
            let m = self.runs[c.run as usize].metrics;
            height = height.max(m.line_height);
            if content.is_none_or(|last| k <= last) {
                ascent = ascent.max(m.ascent);
                descent = descent.max(m.descent);
            }
        }
        if cl.is_empty() {
            let m = self.empty_metrics;
            height = m.line_height;
            ascent = m.ascent;
            descent = m.descent;
        }
        let leading = height - (ascent + descent);
        let baseline = *y + ascent + leading * 0.5;

        let seg_start = self.segs.len() as u32;
        if !cl.is_empty() {
            let para_rtl = self
                .analysis
                .get(text_range.start as usize)
                .is_some_and(|f| f & PARA_RTL != 0);
            self.push_segments(cl, para_rtl as u8);
        }
        *width = width.max(advance - trailing);
        let caret_end = match cl.last() {
            Some(c) if c.newline => c.text.start,
            _ => text_range.end,
        };
        self.lines.push(Line {
            caret_end,
            text: text_range,
            segs: seg_start..self.segs.len() as u32,
            top: *y,
            height,
            baseline,
            ascent,
            descent,
            advance,
            trailing,
            x: 0.0,
        });
        *y += height;
    }

    /// Visual segments of one line's clusters (logical order): UAX #9 L1
    /// resets trailing whitespace, and whitespace before a separator, to
    /// the paragraph level; segments split where the shaped run or the
    /// level changes; L2 reverses them into visual order in place.
    fn push_segments(&mut self, cl: &mut [Cluster], para_level: u8) {
        let mut reset = true;
        for c in cl.iter_mut().rev() {
            if c.l1_sep {
                reset = true;
                c.reset = true;
            } else if c.l1_ws {
                c.reset = reset;
            } else {
                reset = false;
                c.reset = false;
            }
        }
        let first = self.segs.len();
        for c in cl.iter() {
            let level = if c.reset {
                para_level
            } else {
                self.runs[c.run as usize].level
            };
            match self.segs[first..].last_mut() {
                Some(s) if s.run == c.run && s.level == level => {
                    s.text.start = s.text.start.min(c.text.start);
                    s.text.end = s.text.end.max(c.text.end);
                    s.glyphs.start = s.glyphs.start.min(c.glyphs.start);
                    s.glyphs.end = s.glyphs.end.max(c.glyphs.end);
                }
                _ => self.segs.push(Seg {
                    run: c.run,
                    glyphs: c.glyphs.clone(),
                    text: c.text.clone(),
                    level,
                }),
            }
        }
        // L2: from the highest level down to the lowest odd one, reverse
        // every maximal sequence at that level or higher.
        let segs = &mut self.segs[first..];
        let max = segs.iter().map(|s| s.level).max().unwrap_or(0);
        let min_odd = segs.iter().map(|s| s.level | 1).min().unwrap_or(1);
        let mut level = max;
        while level >= min_odd && level > 0 {
            let mut i = 0;
            while i < segs.len() {
                if segs[i].level >= level {
                    let j = i + segs[i..].iter().take_while(|s| s.level >= level).count();
                    segs[i..j].reverse();
                    i = j;
                } else {
                    i += 1;
                }
            }
            level -= 1;
        }
    }

    /// The line holding byte `offset` (the last line for the end).
    pub fn line_of(&self, offset: u32) -> usize {
        self.lines
            .iter()
            .position(|l| offset < l.text.end || (offset == l.text.end && l.text.is_empty()))
            .unwrap_or(self.lines.len().saturating_sub(1))
    }

    /// Whether `seg` places its clusters against their stored order (its
    /// L1 level has another direction than its run).
    fn reversed(&self, seg: &Seg) -> bool {
        (seg.level ^ self.runs[seg.run as usize].level) & 1 == 1
    }

    /// Clusters of segments `segs` (a line's, or one), left to right, read
    /// from the placements: a cluster's left edge is the x of its first
    /// placed glyph, its right edge the next cluster's left edge or, for a
    /// segment's last, the pen after its last glyph. No allocation.
    pub fn clusters_of(&self, segs: Range<u32>) -> LineClusters<'_> {
        LineClusters {
            p: self,
            segs,
            seg: None,
            pending: None,
            prev: None,
        }
    }

    /// A line's clusters left to right (`clusters_of` its segments).
    pub fn line_clusters(&self, line: usize) -> LineClusters<'_> {
        let segs = self.lines.get(line).map_or(0..0, |l| l.segs.clone());
        self.clusters_of(segs)
    }

    /// A line's clusters left to right, with their direction (the
    /// segment's L1 level) and edges from the placements. Newlines are
    /// included (zero width).
    pub fn visual_clusters(&self, line: usize) -> Vec<VisualCluster> {
        self.line_clusters(line).collect()
    }

    /// The left and right edges of a line's content (placements).
    fn line_edges(&self, line: &Line) -> Option<(f32, f32)> {
        let mut it = self.clusters_of(line.segs.clone());
        let first = it.next()?;
        let last = it.last().unwrap_or_else(|| first.clone());
        Some((first.left, last.right))
    }

    /// The caret at byte `offset`: the left edge of the cluster starting
    /// there (its right edge at an odd level), else the line's end.
    pub fn caret(&self, offset: u32) -> Caret {
        let li = self.line_of(offset);
        let Some(line) = self.lines.get(li) else {
            let m = self.empty_metrics;
            return Caret {
                x: 0.0,
                top: 0.0,
                height: m.line_height,
                line: 0,
            };
        };
        let bar = |x: f32| Caret {
            x,
            top: line.top,
            height: line.height,
            line: li as u32,
        };
        for v in self.clusters_of(line.segs.clone()) {
            if v.text.start == offset {
                return bar(if v.rtl { v.right } else { v.left });
            }
        }
        // The end of the line (past its content, before any trailing
        // newline, which has no advance): its visual end.
        bar(match self.line_edges(line) {
            Some((left, right)) => {
                if self.base_rtl {
                    left
                } else {
                    right
                }
            }
            None => line.x,
        })
    }

    /// The cluster under `(x, y)` (paragraph space), from the
    /// placements: None past a line's content or below the last line.
    pub fn cluster_at_point(&self, x: f32, y: f32) -> Option<Range<u32>> {
        let line = self
            .lines
            .iter()
            .position(|l| y >= l.top && y < l.top + l.height)?;
        self.line_clusters(line)
            .find(|v| x >= v.left && x < v.right)
            .map(|v| v.text)
    }

    /// The byte offset nearest `(x, y)` (paragraph space).
    pub fn hit(&self, x: f32, y: f32) -> Hit {
        let Some(li) = self
            .lines
            .iter()
            .position(|l| y < l.top + l.height)
            .or(self.lines.len().checked_sub(1))
        else {
            return Hit {
                offset: 0,
                downstream: true,
            };
        };
        let line = &self.lines[li];
        let mut best: Option<(f32, Hit)> = None;
        {
            for VisualCluster {
                text,
                rtl,
                left,
                right,
            } in self.clusters_of(line.segs.clone())
            {
                if text.is_empty() {
                    continue;
                }
                if x >= left && x < right {
                    // Before or after the cluster, by its nearer half.
                    let after = (x >= left + (right - left) * 0.5) != rtl;
                    let offset = if after { text.end } else { text.start };
                    return Hit {
                        offset,
                        downstream: !after,
                    };
                }
                for (edge, offset) in [
                    (left, if rtl { text.end } else { text.start }),
                    (right, if rtl { text.start } else { text.end }),
                ] {
                    let d = (x - edge).abs();
                    if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                        best = Some((
                            d,
                            Hit {
                                offset,
                                downstream: offset == text.start,
                            },
                        ));
                    }
                }
            }
        }
        match best {
            // Never past a line's newline: the caret stays on the line.
            Some((_, h)) if h.offset > line.caret_end => Hit {
                offset: line.caret_end,
                downstream: false,
            },
            Some((_, h)) => h,
            None => Hit {
                offset: line.text.start,
                downstream: true,
            },
        }
    }

    /// Rectangles covering byte range `range`: one per contiguous visual
    /// stretch per line, full line height.
    pub fn selection_rects(&self, range: Range<u32>) -> Vec<(f32, f32, f32, f32)> {
        let mut out = Vec::new();
        if range.is_empty() {
            return out;
        }
        for line in &self.lines {
            if line.text.end <= range.start || line.text.start >= range.end {
                continue;
            }
            let mut cur: Option<(f32, f32)> = None;
            {
                for VisualCluster {
                    text, left, right, ..
                } in self.clusters_of(line.segs.clone())
                {
                    let inside =
                        text.start >= range.start && text.end <= range.end && !text.is_empty();
                    match (inside, cur.as_mut()) {
                        // Adjacent clusters share an edge exactly: both
                        // read the same placement.
                        (true, Some(c)) if c.1 == left => c.1 = right,
                        (true, _) => {
                            if let Some((a, b)) = cur.take() {
                                out.push((a, line.top, b - a, line.height));
                            }
                            cur = Some((left, right));
                        }
                        (false, _) => {
                            if let Some((a, b)) = cur.take() {
                                out.push((a, line.top, b - a, line.height));
                            }
                        }
                    }
                }
            }
            if let Some((a, b)) = cur {
                out.push((a, line.top, b - a, line.height));
            }
        }
        out
    }

    /// The cluster holding byte `offset`, as its text range: None past
    /// the end. Every byte of a cluster maps to the same range.
    pub fn cluster_at(&self, offset: u32) -> Option<Range<u32>> {
        let r = self.runs.partition_point(|r| r.text.end <= offset);
        let run = self.runs.get(r).filter(|r| r.text.contains(&offset))?;
        let g = &self.glyphs[run.glyphs.start as usize..run.glyphs.end as usize];
        let start = g.iter().map(|g| g.cluster).filter(|&c| c <= offset).max()?;
        let end = g
            .iter()
            .map(|g| g.cluster)
            .filter(|&c| c > offset)
            .min()
            .unwrap_or(run.text.end);
        Some(start..end)
    }

    /// Clusters in logical order as (text range, glyph range): for
    /// comparisons with other shapers.
    pub fn cluster_map(&self) -> Vec<(Range<u32>, Range<u32>)> {
        self.clusters()
            .into_iter()
            .map(|c| (c.text, c.glyphs))
            .collect()
    }

    /// Heap bytes held (capacity).
    pub fn heap_bytes(&self) -> usize {
        self.runs.capacity() * std::mem::size_of::<Run>()
            + self.glyphs.capacity() * std::mem::size_of::<Glyph>()
            + self.lines.capacity() * std::mem::size_of::<Line>()
            + self.segs.capacity() * std::mem::size_of::<Seg>()
            + self.analysis.capacity()
    }
}

/// Shaping state kept across paragraphs: the HarfRust buffer, shape
/// plans and run metrics per font instance, and per-call scratch.
#[derive(Default)]
pub struct Shaper {
    buffer: Option<harfrust::UnicodeBuffer>,
    /// Plans per (instance, right-to-left, script). A plan is compiled
    /// once; HarfRust would compile one per call without it.
    plans: HashMap<(FontInstanceId, bool, [u8; 4]), harfrust::ShapePlan>,
    /// Variation instances per font instance with coordinates.
    instances: HashMap<FontInstanceId, harfrust::ShaperInstance>,
    metrics: HashMap<(FontInstanceId, u32, u32), RunMetrics>,
    /// Scratch: the primary instance per span, and the shaping items.
    primaries: Vec<Option<FontInstanceId>>,
    items: Vec<Item>,
    clusters: Vec<Cluster>,
    /// Grapheme start bytes of the paragraph being shaped.
    graphemes: Vec<u32>,
}

/// A maximal text range with one bidi level, script, font, and span size.
struct Item {
    text: Range<usize>,
    level: u8,
    script: ScriptTag,
    font: Option<FontInstanceId>,
    span: usize,
}

/// Font resolution for one paragraph: the primary instance per style and
/// fallback per uncovered cluster.
pub trait Resolve {
    fn primary(&mut self, family: &str, attrs: FontAttrs) -> Option<FontInstanceId>;
    fn fallback(
        &mut self,
        cluster: &str,
        script: ScriptTag,
        attrs: FontAttrs,
    ) -> Option<FontInstanceId>;
    fn store(&mut self) -> &mut FontStore;
}

/// Run metrics of `font` at `size`; `line_height` > 0 overrides the
/// font's line height. Missing decorations take HarfBuzz's defaults (as
/// Parley): underline one eighteenth of the em below the baseline, the
/// strikeout at half the ascent.
pub fn run_metrics(
    store: &FontStore,
    font: FontInstanceId,
    size: f32,
    line_height: f32,
) -> RunMetrics {
    let inst = store.instance_data(font);
    let Some(f) = store.face_data(inst.face).font() else {
        return RunMetrics::default();
    };
    let coords: Vec<F2Dot14> = inst.coords.iter().map(|&c| F2Dot14::from_bits(c)).collect();
    let m = f.metrics(Size::new(size), LocationRef::new(&coords));
    let default = size / 18.0;
    let (underline_offset, underline_size) = m
        .underline
        .map_or((-default, default), |d| (d.offset, d.thickness));
    let (strike_offset, strike_size) = m
        .strikeout
        .map_or((m.ascent / 2.0, default), |d| (d.offset, d.thickness));
    RunMetrics {
        ascent: m.ascent,
        descent: -m.descent,
        leading: m.leading,
        line_height: if line_height > 0.0 {
            line_height
        } else {
            m.ascent - m.descent + m.leading
        },
        underline_offset,
        underline_size,
        strike_offset,
        strike_size,
    }
}

/// Whether `text` needs the bidi algorithm: any character with a
/// right-to-left or Arabic-number class, or an explicit embedding,
/// override, or isolate. Without one, every level is 0 (W7 turns
/// European numbers left-to-right) and the paragraph is left-to-right.
fn needs_bidi(text: &str) -> bool {
    use BidiClass::*;
    text.chars().any(|c| {
        c >= '\u{0590}'
            && matches!(
                unicode_bidi::bidi_class(c),
                R | AL | AN | RLE | LRE | RLO | LRO | PDF | RLI | LRI | FSI | PDI
            )
    })
}

impl Shaper {
    /// Shapes `spec` into runs and glyphs (logical run order); lines come
    /// from `Paragraph::rewrap`.
    pub fn shape(&mut self, spec: &TextSpec<'_>, fonts: &mut dyn Resolve) -> Paragraph {
        let text = spec.text;
        let default = [SpanStyle {
            start: 0,
            style: TextStyle::default(),
        }];
        let spans = if spec.spans.is_empty() {
            &default[..]
        } else {
            spec.spans
        };
        let mut p = Paragraph {
            text_len: text.len() as u32,
            ..Paragraph::default()
        };
        let attrs = |s: &TextStyle| FontAttrs {
            weight: s.weight,
            italic: s.italic,
        };
        // Each span's primary font was resolved when it was applied; a span
        // without one takes the default family through the same cache.
        self.primaries.clear();
        for s in spans {
            let f = s.style.font.or_else(|| fonts.primary("", attrs(&s.style)));
            self.primaries.push(f);
        }
        let line_height = spans[0].style.line_height;
        if let Some(f) = self.primaries[0] {
            p.empty_metrics = self.run_metrics(fonts.store(), f, spans[0].style.size, line_height);
        }
        if text.is_empty() {
            return p;
        }

        // Analysis kept for line breaking: break opportunities, and with
        // bidi, the paragraph direction and L1 classes.
        p.analysis = vec![0; text.len()];
        for (i, _) in unicode_linebreak::linebreaks(text) {
            if i < text.len() {
                p.analysis[i] |= BREAK;
            }
        }
        let bidi = needs_bidi(text).then(|| BidiInfo::new(text, None));
        if let Some(b) = &bidi {
            use BidiClass::*;
            for para in &b.paragraphs {
                if para.level.is_rtl() {
                    for f in &mut p.analysis[para.range.clone()] {
                        *f |= PARA_RTL;
                    }
                }
            }
            for (i, _) in text.char_indices() {
                p.analysis[i] |= match b.original_classes[i] {
                    WS | FSI | LRI | RLI | PDI | RLE | LRE | RLO | LRO | PDF | BN => L1_WS,
                    B | S => L1_SEP,
                    _ => 0,
                };
            }
        }

        // Items: graphemes grouped by level, script, font, and size.
        // Common and inherited graphemes take the script before them (the
        // first real script for leading ones).
        let script_of = |g: &str| {
            g.chars()
                .map(ScriptTag::of)
                .find(|s| !matches!(&s.0, b"Zyyy" | b"Zinh" | b"Zzzz"))
                .unwrap_or(ScriptTag::COMMON)
        };
        let mut last = text
            .chars()
            .map(ScriptTag::of)
            .find(|s| !matches!(&s.0, b"Zyyy" | b"Zinh" | b"Zzzz"));
        self.items.clear();
        let mut chars = 0usize;
        let mut span = 0usize;
        self.graphemes.clear();
        for (at, g) in text.grapheme_indices(true) {
            self.graphemes.push(at as u32);
            chars += g.chars().count();
            while span + 1 < spans.len() && spans[span + 1].start as usize <= at {
                span += 1;
            }
            let style = spans[span].style;
            let level = bidi.as_ref().map_or(0, |b| b.levels[at].number());
            let script = match script_of(g) {
                ScriptTag::COMMON => last.unwrap_or(ScriptTag::COMMON),
                s => {
                    last = Some(s);
                    s
                }
            };
            // The span's primary face when it covers the grapheme, else
            // fallback.
            let primary = self.primaries[span];
            let font = match primary {
                Some(f) if covers_cluster(fonts.store(), f, g) => Some(f),
                _ => fonts.fallback(g, script, attrs(&style)).or(primary),
            };
            match self.items.last_mut() {
                Some(it)
                    if it.text.end == at
                        && it.level == level
                        && it.script == script
                        && it.font == font
                        && spans[it.span].style.size == style.size =>
                {
                    it.text.end = at + g.len();
                }
                _ => self.items.push(Item {
                    text: at..at + g.len(),
                    level,
                    script,
                    font,
                    span,
                }),
            }
        }

        // Shape each item.
        p.runs.reserve_exact(self.items.len());
        p.glyphs.reserve_exact(chars);
        let mut buffer = self.buffer.take().unwrap_or_default();
        let items = std::mem::take(&mut self.items);
        for it in &items {
            let size = spans[it.span].style.size;
            let glyph_start = p.glyphs.len() as u32;
            let rtl = it.level & 1 == 1;
            let Some(font) = it.font else {
                continue;
            };
            let store = fonts.store();
            let face = store.instance_data(font).face;
            if store.shaper_data(face).is_none() {
                continue;
            }
            let store: &FontStore = store;
            let f = store.face_data(face);
            let Ok(font_ref) = harfrust::FontRef::from_index(f.bytes.as_ref().as_ref(), f.index)
            else {
                continue;
            };
            let coords = &store.instance_data(font).coords;
            let instance = if coords.is_empty() {
                None
            } else {
                Some(&*self.instances.entry(font).or_insert_with(|| {
                    harfrust::ShaperInstance::from_coords(
                        &font_ref,
                        coords.iter().map(|&c| F2Dot14::from_bits(c)),
                    )
                }))
            };
            let shaper = store
                .shaper(face)
                .shaper(&font_ref)
                .instance(instance)
                .build();
            let direction = if rtl {
                harfrust::Direction::RightToLeft
            } else {
                harfrust::Direction::LeftToRight
            };
            let script = harfrust::Script::from_iso15924_tag(harfrust::Tag::new(&it.script.0));
            let plan = self
                .plans
                .entry((font, rtl, it.script.0))
                .or_insert_with(|| harfrust::ShapePlan::new(&shaper, direction, script, None, &[]));
            // No pre/post context, as Parley: items split only where
            // shaping cannot join anyway (script, font, level, size).
            let range = it.text.clone();
            buffer.push_str(&text[range.clone()]);
            buffer.set_direction(direction);
            if let Some(s) = script {
                buffer.set_script(s);
            }
            let out = shaper.shape(
                buffer,
                harfrust::ShapeOptions::new()
                    .plan(Some(plan))
                    .point_size(Some(size)),
            );
            let upem = f.units_per_em as f32;
            let scale = size / upem;
            let mut span = Paragraph::span_at(spans, range.start as u32);
            for (info, pos) in out.glyph_infos().iter().zip(out.glyph_positions()) {
                // Clusters start at graphemes: HarfRust keeps controls
                // (CR and LF of a CRLF) apart; a grapheme is one cluster.
                let cluster = range.start as u32 + info.cluster;
                let g = self.graphemes.partition_point(|&b| b <= cluster) - 1;
                let cluster = self.graphemes[g];
                let c = text[cluster as usize..].chars().next().unwrap_or(' ');
                let newline = is_newline(c);
                // Spans in a run: at most a few; clusters are monotonic
                // per direction, so look up only on a change.
                if !(spans[span].start <= cluster
                    && spans.get(span + 1).is_none_or(|n| cluster < n.start))
                {
                    span = Paragraph::span_at(spans, cluster);
                }
                p.glyphs.push(Glyph {
                    id: if newline {
                        HIDDEN
                    } else {
                        info.glyph_id as u16
                    },
                    style: span as u16,
                    cluster,
                    advance: if newline {
                        0.0
                    } else {
                        pos.x_advance as f32 * scale
                    },
                    dx: pos.x_offset as f32 * scale,
                    dy: -pos.y_offset as f32 * scale,
                    x: 0.0,
                    y: 0.0,
                });
            }
            buffer = out.clear();
            // Letter spacing: each cluster's last glyph (stored order)
            // advances by its span's spacing (Parley's rule).
            let run_glyphs = &mut p.glyphs[glyph_start as usize..];
            let mut g = 0;
            while g < run_glyphs.len() {
                let c = run_glyphs[g].cluster;
                let mut e = g + 1;
                while e < run_glyphs.len() && run_glyphs[e].cluster == c {
                    e += 1;
                }
                let last = &mut run_glyphs[e - 1];
                let spacing = spans[last.style as usize].style.letter_spacing;
                if spacing != 0.0 && last.id != HIDDEN {
                    last.advance += spacing;
                }
                g = e;
            }
            let metrics = self.run_metrics(store, font, size, line_height);
            p.runs.push(Run {
                text: range.start as u32..range.end as u32,
                font,
                size,
                level: it.level,
                glyphs: glyph_start..p.glyphs.len() as u32,
                metrics,
            });
        }
        self.items = items;
        self.buffer = Some(buffer);
        p.mark_clusters(text, &mut self.clusters);
        p
    }

    /// `run_metrics`, cached per (instance, size, line height).
    fn run_metrics(
        &mut self,
        store: &FontStore,
        font: FontInstanceId,
        size: f32,
        line_height: f32,
    ) -> RunMetrics {
        *self
            .metrics
            .entry((font, size.to_bits(), line_height.to_bits()))
            .or_insert_with(|| run_metrics(store, font, size, line_height))
    }
}
