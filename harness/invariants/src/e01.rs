//! E01 (ARCHITECTURE.md §5): the owned paragraph against Parley, the
//! oracle, on the pinned fonts. Both sides get the same font bytes and
//! the same fallback order; `Snapshot` is what each side's layout says
//! about line breaks, cluster mappings, glyph positions, and line
//! metrics, so `compare` finds the first difference of each kind.

use std::borrow::Cow;

use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

use craie_text::TextEngine;
use craie_text::fonts;
use craie_text::paragraph::{HIDDEN, Paragraph, SpanStyle, TextSpec, TextStyle};
use parley::fontique::{Blob, Collection, CollectionOptions, SourceCache};
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontStyle, FontWeight, Layout,
    LayoutContext, PositionedLayoutItem, StyleProperty,
};

/// The pinned families in fallback order: Parley's font stack, and
/// `fonts::pinned`'s registration order.
pub const STACK: &str = "Noto Sans, Noto Sans Arabic, Noto Sans Hebrew, Noto Sans Devanagari, \
                         Noto Sans JP, Noto Sans Symbols 2, Noto Emoji";

/// One E01 case: text, styled spans (byte start, style), and the wrap
/// widths it is laid out at.
pub struct Case {
    pub name: &'static str,
    /// Multilingual, styled, wrapped, bidi (the E01 entry's classes),
    /// and breaks, emoji, fallback, and missing (round 1 additions).
    pub class: &'static str,
    pub text: String,
    pub spans: Vec<SpanStyle>,
    pub widths: Vec<Option<f32>>,
    /// Some character has no pinned face: both sides draw `.notdef`.
    pub notdef: bool,
}

fn span(start: usize, size: f32, weight: u16, italic: bool) -> SpanStyle {
    SpanStyle {
        start: start as u32,
        style: TextStyle {
            size,
            weight,
            italic,
        },
    }
}

fn plain(size: f32) -> Vec<SpanStyle> {
    vec![span(0, size, 400, false)]
}

/// The E01 cases.
pub fn cases() -> Vec<Case> {
    let pangram = "The quick brown fox jumps over the lazy dog. ";
    let long: String = pangram.repeat(6)
        + "Pack my box with five dozen liquor jugs.\nA second paragraph line, then \
           Supercalifragilisticexpialidocious overflows.";
    let styled = "Plain, bold words, italic words, big text, and plain again at the end.";
    let b = styled.find("bold").unwrap();
    let i = styled.find("italic").unwrap();
    let g = styled.find("big").unwrap();
    let a = styled.find(", and").unwrap();
    vec![
        Case {
            name: "latin",
            class: "wrapped",
            text: long.clone(),
            spans: plain(14.0),
            notdef: false,
            widths: vec![None, Some(120.0), Some(240.0), Some(480.0)],
        },
        Case {
            name: "latin-narrow",
            class: "wrapped",
            text: long,
            spans: plain(17.0),
            notdef: false,
            widths: vec![Some(40.0), Some(90.0)],
        },
        Case {
            name: "styled",
            class: "styled",
            text: styled.to_string(),
            spans: vec![
                span(0, 14.0, 400, false),
                span(b, 14.0, 700, false),
                span(i, 14.0, 400, true),
                span(g, 24.0, 400, false),
                span(a, 14.0, 400, false),
            ],
            notdef: false,
            widths: vec![None, Some(150.0), Some(300.0)],
        },
        Case {
            name: "styled-synth",
            class: "styled",
            // Bold and italic Arabic: no such faces, synthesis on both sides.
            text: "Bold مرحبا and italic שלום".to_string(),
            spans: vec![
                span(0, 16.0, 700, false),
                span("Bold مرحبا ".len(), 16.0, 400, true),
            ],
            notdef: false,
            widths: vec![None, Some(80.0)],
        },
        Case {
            name: "multilingual",
            class: "multilingual",
            text: "Hello नमस्ते दुनिया こんにちは東京 ✕ done — क्षत्रिय 日本語のテキスト".to_string(),
            spans: plain(16.0),
            notdef: false,
            widths: vec![None, Some(100.0), Some(200.0)],
        },
        Case {
            name: "bidi-ltr",
            class: "bidi",
            text: "Hello שלום world مرحبا بالعالم 123 end.".to_string(),
            spans: plain(15.0),
            notdef: false,
            widths: vec![None, Some(90.0), Some(160.0)],
        },
        Case {
            name: "bidi-rtl",
            class: "bidi",
            text: "مرحبا بالعالم with English inside and ٣٤٥ numbers، ثم نهاية الجملة.".to_string(),
            spans: plain(15.0),
            notdef: false,
            widths: vec![None, Some(110.0), Some(220.0)],
        },
        Case {
            name: "breaks",
            class: "breaks",
            // Hard breaks Parley handles as UAX #14 does. CRLF, NEL, and
            // no-break spaces, where Parley differs, have their own tests
            // (`tests/e01_text.rs`).
            text: "one\ntwo\rthree\u{2028}four\u{2029}five and the end".to_string(),
            spans: plain(15.0),
            notdef: false,
            widths: vec![None, Some(0.0), Some(25.0), Some(60.0)],
        },
        Case {
            name: "emoji",
            class: "emoji",
            text: "Hi 👨\u{200D}👩\u{200D}👧 and 🏳\u{FE0F}\u{200D}🌈, 👍🏽 ok 1\u{FE0F}\u{20E3} \
                   #\u{FE0F}\u{20E3} 🇯🇵 ✅ ❤\u{FE0F} 🙂🚀"
                .to_string(),
            spans: plain(16.0),
            notdef: false,
            widths: vec![None, Some(60.0), Some(120.0)],
        },
        Case {
            name: "fallback",
            class: "fallback",
            // Marks the primary lacks: the whole cluster falls back. A
            // base the primary covers with a mark it lacks, where Parley
            // splits the fonts, has its own test.
            text: "a\u{0301}\u{0323} 1\u{0651} \u{25CC} ✕ 1\u{20E3}".to_string(),
            spans: plain(16.0),
            notdef: false,
            widths: vec![None, Some(50.0)],
        },
        Case {
            name: "missing",
            class: "missing",
            // No pinned face covers Thai or Ethiopic.
            text: "Thai สวัสดี and Geez ሰላም end".to_string(),
            spans: plain(15.0),
            notdef: true,
            widths: vec![None, Some(60.0)],
        },
        Case {
            name: "hebrew-lines",
            class: "bidi",
            text: "שורה ראשונה\nשורה שנייה עם עוד מילים\n\nסוף".to_string(),
            spans: plain(15.0),
            notdef: false,
            widths: vec![None, Some(70.0)],
        },
    ]
}

/// A glyph group in logical order: a text range and its glyph ids
/// (shaper order), with its advance.
#[derive(Clone, Debug, PartialEq)]
pub struct ClusterSnap {
    pub text: Range<usize>,
    pub glyphs: Vec<u16>,
    pub advance: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineSnap {
    pub text: Range<usize>,
    pub top: f32,
    pub height: f32,
    pub baseline: f32,
    pub advance: f32,
    pub trailing: f32,
    pub x: f32,
    /// Drawn glyphs in visual order, paragraph space.
    pub glyphs: Vec<Drawn>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drawn {
    pub id: u16,
    pub x: f32,
    pub y: f32,
    /// Byte offset of the glyph's cluster (its grapheme's start).
    pub cluster: usize,
    pub advance: f32,
    /// Index of the pinned file the glyph comes from (`usize::MAX`:
    /// another font).
    pub font: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Oracle clusters merged because they split one grapheme (UAX #29,
    /// GB9c conjuncts; Parley clusters finer). Always 0 for ours.
    pub grapheme_merges: usize,
    /// Oracle lines whose `LineMetrics::advance` is not the sum of their
    /// glyph advances (Parley with emoji ZWJ ligatures); the glyph sum is
    /// used. Always 0 for ours.
    pub advance_fixups: usize,
    pub rtl: bool,
    pub width: f32,
    pub height: f32,
    pub lines: Vec<LineSnap>,
    pub clusters: Vec<ClusterSnap>,
}

fn newline(text: &str, r: &Range<usize>) -> bool {
    text[r.clone()].chars().any(|c| {
        matches!(
            c,
            '\n' | '\r' | '\u{2028}' | '\u{2029}' | '\u{0B}' | '\u{0C}' | '\u{85}'
        )
    })
}

/// The pinned file `bytes` points into (`usize::MAX`: none).
fn pinned_index(bytes: &[u8]) -> usize {
    fonts::pinned_files()
        .iter()
        .position(|f| f.as_ptr() == bytes.as_ptr())
        .unwrap_or(usize::MAX)
}

/// Grapheme start bytes of `text`.
fn grapheme_starts(text: &str) -> Vec<usize> {
    text.grapheme_indices(true).map(|(i, _)| i).collect()
}

/// The owned paragraph's snapshot. Drawn glyphs follow the placement
/// order: a segment whose L1 level differs from its run's direction
/// places its clusters in reverse.
pub fn ours(engine: &TextEngine, p: &Paragraph, text: &str) -> Snapshot {
    let store = &engine.fonts.store;
    let font_of = |run: u32| {
        let face = store.instance_data(p.runs[run as usize].font).face;
        pinned_index(store.face_data(face).bytes.as_ref().as_ref())
    };
    let clusters = p
        .cluster_map()
        .into_iter()
        .map(|(t, g)| {
            let glyphs = &p.glyphs[g.start as usize..g.end as usize];
            let t = t.start as usize..t.end as usize;
            ClusterSnap {
                glyphs: if newline(text, &t) {
                    Vec::new()
                } else {
                    glyphs.iter().map(|g| g.id).collect()
                },
                advance: glyphs.iter().map(|g| g.advance).sum(),
                text: t,
            }
        })
        .collect();
    let lines = p
        .lines
        .iter()
        .map(|l| {
            let mut glyphs = Vec::new();
            for s in &p.segs[l.segs.start as usize..l.segs.end as usize] {
                let font = font_of(s.run);
                // Cluster groups in stored order; reversed when the
                // segment's level has another direction than its run.
                let mut groups: Vec<&[craie_text::paragraph::Glyph]> = p.glyphs
                    [s.glyphs.start as usize..s.glyphs.end as usize]
                    .chunk_by(|a, b| a.cluster == b.cluster)
                    .collect();
                if (s.level ^ p.runs[s.run as usize].level) & 1 == 1 {
                    groups.reverse();
                }
                for g in groups.into_iter().flatten() {
                    if g.id != HIDDEN {
                        glyphs.push(Drawn {
                            id: g.id,
                            x: g.x + g.dx,
                            y: g.y,
                            cluster: g.cluster as usize,
                            advance: g.advance,
                            font,
                        });
                    }
                }
            }
            LineSnap {
                text: l.text.start as usize..l.text.end as usize,
                top: l.top,
                height: l.height,
                baseline: l.baseline,
                advance: l.advance,
                trailing: l.trailing,
                x: l.x,
                glyphs,
            }
        })
        .collect();
    Snapshot {
        grapheme_merges: 0,
        advance_fixups: 0,
        rtl: p.base_rtl,
        width: p.width,
        height: p.height,
        lines,
        clusters,
    }
}

/// Parley's snapshot. Parley splits a ligature over its source clusters
/// (glyphs on the first, none on the rest): those merge back into one
/// group, as a shaper cluster. Clusters that split a grapheme merge too
/// (counted in `grapheme_merges`), and a drawn glyph's cluster is its
/// grapheme's start.
pub fn parley(layout: &Layout<u16>, text: &str) -> Snapshot {
    let starts = grapheme_starts(text);
    let grapheme = |b: usize| starts[starts.partition_point(|&s| s <= b) - 1];
    let mut clusters: Vec<ClusterSnap> = Vec::new();
    let mut lines = Vec::new();
    let mut advance_fixups = 0;
    let mut width = 0.0f32;
    for line in layout.lines() {
        let m = line.metrics();
        let mut glyphs = Vec::new();
        // The line's advance from its glyphs (hard breaks excluded).
        let mut advance = 0.0f32;
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                continue;
            };
            let mut x = run.offset();
            let y = run.baseline();
            let font = pinned_index(run.run().font().data.data());
            for c in run.run().visual_clusters() {
                let hard = c.is_hard_line_break() || newline(text, &c.text_range());
                for g in c.glyphs() {
                    if !hard {
                        advance += g.advance;
                        glyphs.push(Drawn {
                            id: g.id as u16,
                            x: x + g.x,
                            y: y + g.y,
                            cluster: grapheme(c.text_range().start),
                            advance: g.advance,
                            font,
                        });
                    }
                    x += g.advance;
                }
            }
        }
        for run in line.runs() {
            for c in run.clusters() {
                let t = c.text_range();
                let ids: Vec<u16> = c.glyphs().map(|g| g.id as u16).collect();
                let advance: f32 = c.glyphs().map(|g| g.advance).sum();
                let merge = ids.is_empty()
                    && clusters
                        .last()
                        .is_some_and(|l| l.text.end == t.start && !newline(text, &t));
                if merge {
                    let last = clusters.last_mut().unwrap();
                    last.text.end = t.end;
                    last.advance += advance;
                } else {
                    clusters.push(ClusterSnap {
                        glyphs: if newline(text, &t) { Vec::new() } else { ids },
                        advance,
                        text: t,
                    });
                }
            }
        }
        if (advance - m.advance).abs() > 1e-3 * advance.abs().max(1.0) {
            advance_fixups += 1;
        }
        width = width.max(advance - m.trailing_whitespace);
        lines.push(LineSnap {
            text: line.text_range(),
            // The line box top. (`block_min_coord` is the selection
            // box: with negative leading, Parley clamps it to ascent plus
            // descent; ours stays the line box.)
            top: m.baseline - m.ascent - m.leading * 0.5,
            height: m.line_height,
            baseline: m.baseline,
            advance,
            trailing: m.trailing_whitespace,
            x: m.offset,
            glyphs,
        });
    }
    // Clusters of one run split over lines arrive in line order; within a
    // line, `runs()` is visual order. Logical order is by text start.
    clusters.sort_by_key(|c| c.text.start);
    let mut merged: Vec<ClusterSnap> = Vec::with_capacity(clusters.len());
    let mut grapheme_merges = 0;
    for c in clusters {
        match merged.last_mut() {
            Some(last)
                if grapheme(c.text.start) != c.text.start && last.text.end == c.text.start =>
            {
                grapheme_merges += 1;
                last.text.end = c.text.end;
                last.glyphs.extend(c.glyphs);
                last.advance += c.advance;
            }
            _ => merged.push(c),
        }
    }
    let clusters = merged;
    if advance_fixups == 0 {
        width = layout.width();
    }
    Snapshot {
        grapheme_merges,
        advance_fixups,
        rtl: layout.is_rtl(),
        width,
        height: layout.height(),
        lines,
        clusters,
    }
}

/// Differences between two snapshots.
#[derive(Clone, Debug, Default)]
pub struct Diff {
    /// Lines whose text range differs (or a line count difference).
    pub line_breaks: usize,
    /// Glyph groups whose text range or glyph ids differ.
    pub clusters: usize,
    pub first_cluster: Option<(ClusterSnap, Option<ClusterSnap>)>,
    pub first_line: Option<(Range<usize>, Option<Range<usize>>)>,
    /// Lines whose drawn glyphs (id, source cluster, font file) differ
    /// in visual order, after the L1 correction below.
    pub glyph_ids: usize,
    /// Lines where only UAX #9 rule L1 separates the two: the line's
    /// trailing whitespace sits at the paragraph's end side in ours, and
    /// inside its embedded run in Parley's.
    pub l1_lines: usize,
    /// Largest absolute differences, logical points.
    pub advance: f32,
    pub position: f32,
    pub metrics: f32,
    /// Largest difference over its bound (`bound`); at most 1 passes.
    pub worst: f64,
}

impl Diff {
    pub fn exact_structure(&self) -> bool {
        self.line_breaks == 0 && self.clusters == 0 && self.glyph_ids == 0
    }

    /// Structure equal, advances bit-equal (both sides run HarfRust 0.12
    /// with the same scale), and every position and metric within its
    /// bound.
    pub fn pass(&self) -> bool {
        self.exact_structure() && self.advance == 0.0 && self.worst <= 1.0
    }

    fn check(&mut self, field: Field, ours: f32, oracle: f32, bound: f64) {
        let e = (ours - oracle).abs();
        match field {
            Field::Position => self.position = self.position.max(e),
            Field::Metric => self.metrics = self.metrics.max(e),
        }
        self.worst = self.worst.max(e as f64 / bound);
    }
}

enum Field {
    Position,
    Metric,
}

/// gamma(k) = k·u / (1 − k·u), u = 2^-24: the relative error bound of k
/// f32 additions of nonnegative terms (Higham, §3.1).
fn gamma(k: usize) -> f64 {
    let ku = k as f64 * f64::from(f32::EPSILON) / 2.0;
    ku / (1.0 - ku)
}

/// The largest difference two f32 sums of the same nonnegative terms
/// can show when each takes at most `k` additions in any order: each is
/// within gamma(k)·S of the exact sum, S the sum of the terms'
/// magnitudes. One extra ulp of `S` covers the final rounding of a
/// difference such as `baseline − ascent`.
fn bound(k: usize, s: f32) -> f64 {
    let s = f64::from(s.abs()).max(f64::MIN_POSITIVE);
    2.0 * gamma(k) * s + s * f64::from(f32::EPSILON)
}

/// Parley's line with UAX #9 rule L1 applied to the trailing whitespace
/// at the line end: those glyphs move to the paragraph's end side (right
/// for LTR, left for RTL) and the glyphs they passed shift by their
/// advance. `None` if the trailing whitespace glyphs are not one
/// contiguous block (then no correction applies).
fn apply_l1(line: &LineSnap, text: &str, rtl: bool) -> Option<Vec<Drawn>> {
    let s = &text[line.text.clone()];
    let ws = line.text.start + s.trim_end_matches(char::is_whitespace).len();
    let moved: Vec<usize> = (0..line.glyphs.len())
        .filter(|&i| line.glyphs[i].cluster >= ws)
        .collect();
    let (&first, &last) = (moved.first()?, moved.last()?);
    if last - first + 1 != moved.len() {
        return None;
    }
    let w: f32 = moved.iter().map(|&i| line.glyphs[i].advance).sum();
    let block = &line.glyphs[first..=last];
    let (before, after) = (&line.glyphs[..first], &line.glyphs[last + 1..]);
    let mut out = Vec::with_capacity(line.glyphs.len());
    if rtl {
        // The block moves to the left end; what was left of it shifts right.
        let left = before.first().map_or(block[0].x, |g| g.x);
        let dx = left - block[0].x;
        out.extend(block.iter().map(|g| Drawn { x: g.x + dx, ..*g }));
        out.extend(before.iter().map(|g| Drawn { x: g.x + w, ..*g }));
        out.extend_from_slice(after);
    } else {
        let right = after
            .last()
            .map_or(block[block.len() - 1].x, |g| g.x + g.advance - w);
        let dx = right - block[0].x;
        out.extend_from_slice(before);
        out.extend(after.iter().map(|g| Drawn { x: g.x - w, ..*g }));
        out.extend(block.iter().map(|g| Drawn { x: g.x + dx, ..*g }));
    }
    Some(out)
}

/// Compares `a` (ours) with `b` (the oracle) laid out from `text`.
pub fn compare(a: &Snapshot, b: &Snapshot, text: &str) -> Diff {
    let mut d = Diff::default();
    let n = a.lines.len().max(b.lines.len());
    // A drawn glyph is its id, source cluster, and font file.
    let ids = |g: &[Drawn]| {
        g.iter()
            .map(|g| (g.id, g.cluster, g.font))
            .collect::<Vec<_>>()
    };
    for i in 0..n {
        match (a.lines.get(i), b.lines.get(i)) {
            (Some(x), Some(y)) => {
                if x.text != y.text {
                    d.line_breaks += 1;
                    d.first_line
                        .get_or_insert((x.text.clone(), Some(y.text.clone())));
                    continue;
                }
                // Line i's top and baseline sum the heights of lines
                // 0..i and its own metrics: i + 4 additions at most.
                let k = i + 4;
                let s = x.top + x.height + x.baseline.abs();
                for (p, q) in [
                    (x.top, y.top),
                    (x.height, y.height),
                    (x.baseline, y.baseline),
                ] {
                    d.check(Field::Metric, p, q, bound(k, s));
                }
                let n = x.glyphs.len();
                // Line advance and offset: a sum over the line's glyphs
                // and the alignment offset.
                let s = x.advance + x.x.abs() + x.trailing;
                d.check(Field::Metric, x.advance, y.advance, bound(n + 2, s));
                d.check(Field::Metric, x.x, y.x, bound(n + 3, s));
                let mut oracle = y.glyphs.clone();
                let mut extra = 0;
                if ids(&x.glyphs) != ids(&oracle) {
                    match apply_l1(y, text, b.rtl) {
                        Some(g) if ids(&g) == ids(&x.glyphs) => {
                            d.l1_lines += 1;
                            oracle = g;
                            // The correction adds its block sum.
                            extra = n + 1;
                        }
                        _ => {
                            d.glyph_ids += 1;
                            continue;
                        }
                    }
                } else {
                    // Parley counts trailing whitespace only when it is
                    // the visual last run; under L1 it always is.
                    d.check(Field::Metric, x.trailing, y.trailing, bound(n + 2, s));
                }
                // Glyph j: the line offset plus j advances plus its own
                // shaping offset (x); the baseline plus its offset (y).
                let sy = x.top + x.height + x.baseline.abs();
                for (j, (g, h)) in x.glyphs.iter().zip(&oracle).enumerate() {
                    let sx = x.x.abs() + x.advance + g.advance;
                    d.check(Field::Position, g.x, h.x, bound(j + 3 + extra, sx));
                    d.check(
                        Field::Position,
                        g.y,
                        h.y,
                        bound(i + 5, sy + (g.y - x.baseline).abs()),
                    );
                }
            }
            (Some(x), None) => {
                d.line_breaks += 1;
                d.first_line.get_or_insert((x.text.clone(), None));
            }
            (None, Some(_)) => d.line_breaks += 1,
            (None, None) => {}
        }
    }
    for i in 0..a.clusters.len().max(b.clusters.len()) {
        match (a.clusters.get(i), b.clusters.get(i)) {
            (Some(x), Some(y)) if x.text == y.text && x.glyphs == y.glyphs => {
                d.advance = d.advance.max((x.advance - y.advance).abs());
            }
            (x, y) => {
                d.clusters += 1;
                if let Some(x) = x {
                    d.first_cluster.get_or_insert((x.clone(), y.cloned()));
                }
            }
        }
    }
    let lines = a.lines.len();
    d.check(
        Field::Metric,
        a.height,
        b.height,
        bound(lines + 4, a.height),
    );
    if d.l1_lines == 0 {
        // Parley's width counts L1 lines' embedded trailing whitespace.
        let n = a.lines.iter().map(|l| l.glyphs.len()).max().unwrap_or(0);
        d.check(Field::Metric, a.width, b.width, bound(n + 3, a.width));
    }
    d
}

/// Parley over the pinned fonts only, with `STACK` as every span's
/// family.
pub struct Oracle {
    font_cx: FontContext,
    layout_cx: LayoutContext<u16>,
}

impl Oracle {
    pub fn new() -> Oracle {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        for bytes in fonts::pinned_files() {
            collection.register_fonts(Blob::new(Arc::new(bytes)), None);
        }
        Oracle {
            font_cx: FontContext {
                collection,
                source_cache: SourceCache::default(),
            },
            layout_cx: LayoutContext::new(),
        }
    }

    /// Lays `case`'s text out at `width`, spans as Parley ranges whose
    /// brush is the span index.
    pub fn layout(&mut self, text: &str, spans: &[SpanStyle], width: Option<f32>) -> Layout<u16> {
        let mut b = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, false);
        b.push_default(StyleProperty::FontFamily(FontFamily::Source(
            Cow::Borrowed(STACK),
        )));
        for (k, s) in spans.iter().enumerate() {
            let end = spans.get(k + 1).map_or(text.len(), |n| n.start as usize);
            let r = s.start as usize..end;
            b.push(StyleProperty::FontSize(s.style.size), r.clone());
            b.push(
                StyleProperty::FontWeight(FontWeight::new(s.style.weight as f32)),
                r.clone(),
            );
            let style = if s.style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            };
            b.push(StyleProperty::FontStyle(style), r.clone());
            b.push(StyleProperty::Brush(k as u16), r);
        }
        let mut layout = b.build(text);
        layout.break_all_lines(width);
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    /// Lays out again at another width, as a width change would.
    pub fn rewrap(&mut self, layout: &mut Layout<u16>, width: Option<f32>) {
        layout.break_all_lines(width);
        layout.align(Alignment::Start, AlignmentOptions::default());
    }
}

impl Default for Oracle {
    fn default() -> Oracle {
        Oracle::new()
    }
}

/// An owned engine over the pinned fonts.
pub fn engine() -> TextEngine {
    TextEngine::with_source(Box::new(fonts::pinned()))
}

/// The owned layout of `text` at `width`.
pub fn layout(
    engine: &mut TextEngine,
    text: &str,
    spans: &[SpanStyle],
    width: Option<f32>,
) -> Paragraph {
    let spec = TextSpec {
        text,
        family: "Noto Sans",
        spans,
    };
    engine.layout_text(&spec, width)
}
