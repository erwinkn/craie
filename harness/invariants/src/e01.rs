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
    /// Oracle lines per width compared by their glyph sum (Parley's
    /// emoji advance defect); empty: none at any width.
    pub advance_fixups: Vec<usize>,
    /// Oracle clusters merged per layout because Parley splits a
    /// grapheme (GB9c conjuncts).
    pub grapheme_merges: usize,
}

fn span(start: usize, size: f32, weight: u16, italic: bool) -> SpanStyle {
    SpanStyle {
        start: start as u32,
        style: TextStyle {
            size,
            weight,
            italic,
            ..TextStyle::default()
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
            advance_fixups: vec![],
            grapheme_merges: 0,
            widths: vec![None, Some(120.0), Some(240.0), Some(480.0)],
        },
        Case {
            name: "latin-narrow",
            class: "wrapped",
            text: long,
            spans: plain(17.0),
            notdef: false,
            advance_fixups: vec![],
            grapheme_merges: 0,
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
            advance_fixups: vec![],
            grapheme_merges: 0,
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
            advance_fixups: vec![],
            grapheme_merges: 0,
            widths: vec![None, Some(80.0)],
        },
        Case {
            name: "multilingual",
            class: "multilingual",
            text: "Hello नमस्ते दुनिया こんにちは東京 ✕ done — क्षत्रिय 日本語のテキスト".to_string(),
            spans: plain(16.0),
            notdef: false,
            advance_fixups: vec![],
            grapheme_merges: 1,
            widths: vec![None, Some(100.0), Some(200.0)],
        },
        Case {
            name: "bidi-ltr",
            class: "bidi",
            text: "Hello שלום world مرحبا بالعالم 123 end.".to_string(),
            spans: plain(15.0),
            notdef: false,
            advance_fixups: vec![],
            grapheme_merges: 0,
            widths: vec![None, Some(90.0), Some(160.0)],
        },
        Case {
            name: "bidi-rtl",
            class: "bidi",
            text: "مرحبا بالعالم with English inside and ٣٤٥ numbers، ثم نهاية الجملة.".to_string(),
            spans: plain(15.0),
            notdef: false,
            advance_fixups: vec![],
            grapheme_merges: 0,
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
            advance_fixups: vec![],
            grapheme_merges: 0,
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
            advance_fixups: vec![1, 2, 2],
            grapheme_merges: 0,
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
            advance_fixups: vec![],
            grapheme_merges: 0,
            widths: vec![None, Some(50.0)],
        },
        Case {
            name: "missing",
            class: "missing",
            // No pinned face covers Thai or Ethiopic.
            text: "Thai สวัสดี and Geez ሰላም end".to_string(),
            spans: plain(15.0),
            notdef: true,
            advance_fixups: vec![],
            grapheme_merges: 0,
            widths: vec![None, Some(60.0)],
        },
        Case {
            name: "ligatures",
            class: "ligature",
            // Latin ffi/fi ligatures across graphemes, one with a mark,
            // and marked Arabic (Noto Sans Arabic draws lam-alef as two
            // glyphs; Parley lists its marked clusters' glyph-less
            // continuation before the start). Narrow widths: Parley breaks
            // inside a marked grapheme, its own test.
            text: "office fi\u{0301}ne لا لَا بِلَا end".to_string(),
            spans: plain(16.0),
            notdef: false,
            advance_fixups: vec![],
            grapheme_merges: 0,
            widths: vec![None],
        },
        Case {
            name: "spacing",
            class: "styled",
            // Letter spacing on one span, an absolute line height for the
            // paragraph (no ligatures: spacing splits them differently).
            text: "The quick brown dog jumps over the lazy cat, then walks home.".to_string(),
            spans: {
                let t = "The quick brown dog jumps over the lazy cat, then walks home.";
                let (b, d) = (t.find("brown").unwrap(), t.find(" dog").unwrap());
                vec![
                    SpanStyle {
                        start: 0,
                        style: TextStyle {
                            size: 15.0,
                            line_height: 26.0,
                            ..TextStyle::default()
                        },
                    },
                    SpanStyle {
                        start: b as u32,
                        style: TextStyle {
                            size: 15.0,
                            letter_spacing: 2.5,
                            ..TextStyle::default()
                        },
                    },
                    SpanStyle {
                        start: d as u32,
                        style: TextStyle {
                            size: 15.0,
                            ..TextStyle::default()
                        },
                    },
                ]
            },
            notdef: false,
            advance_fixups: vec![],
            grapheme_merges: 0,
            widths: vec![None, Some(120.0), Some(200.0)],
        },
        Case {
            name: "hebrew-lines",
            class: "bidi",
            text: "שורה ראשונה\nשורה שנייה עם עוד מילים\n\nסוף".to_string(),
            spans: plain(15.0),
            notdef: false,
            advance_fixups: vec![],
            grapheme_merges: 0,
            widths: vec![None, Some(70.0)],
        },
    ]
}

/// Letter spacing that starts inside a ligature (office's ffi) and
/// covers whole ones (affix). Not in `cases`: spaced runs drop optional
/// ligatures (CSS), Parley keeps them; its own test asserts exactly that
/// difference.
pub fn spaced_ligatures() -> Case {
    Case {
        name: "spacing-ligatures",
        class: "ligature",
        text: "office fine affix waffle".to_string(),
        spans: {
            let t = "office fine affix waffle";
            let at = |s: &str| t.find(s).unwrap() as u32;
            let style = |letter_spacing: f32| TextStyle {
                size: 16.0,
                letter_spacing,
                ..TextStyle::default()
            };
            vec![
                SpanStyle {
                    start: 0,
                    style: style(0.0),
                },
                SpanStyle {
                    start: at("fice"),
                    style: style(2.0),
                },
                SpanStyle {
                    start: at(" fine"),
                    style: style(0.0),
                },
                SpanStyle {
                    start: at("affix"),
                    style: style(1.5),
                },
                SpanStyle {
                    start: at(" waffle"),
                    style: style(0.0),
                },
            ]
        },
        notdef: false,
        advance_fixups: vec![],
        grapheme_merges: 0,
        widths: vec![None, Some(60.0)],
    }
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
    /// The line's advance as its layout reports it (Parley's
    /// `LineMetrics::advance`, raw).
    pub advance: f32,
    pub trailing: f32,
    pub x: f32,
    /// Drawn glyphs in visual order, paragraph space.
    pub glyphs: Vec<Drawn>,
    /// Oracle only: the sum of the line's glyph advances, and whether the
    /// line is a verified case of Parley's emoji advance defect (it holds
    /// a multi-codepoint emoji sequence, and Parley's own run advances
    /// sum to its glyph advances). Only such a line may be compared by
    /// its glyph sum (`compare`).
    pub glyph_advance: f32,
    pub emoji_defect: bool,
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

/// The pinned file `bytes` holds (`usize::MAX`: none). By content: in
/// release builds a crate that inlines `pinned_files` can hold its own
/// copy of the bytes, so addresses differ.
fn pinned_index(bytes: &[u8]) -> usize {
    fonts::pinned_files()
        .iter()
        .position(|f| f.as_ptr() == bytes.as_ptr() || *f == bytes)
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
                glyph_advance: l.advance,
                emoji_defect: false,
            }
        })
        .collect();
    Snapshot {
        grapheme_merges: 0,
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
    // Parley's clusters with (ligature continuation, right-to-left run).
    let mut clusters: Vec<(ClusterSnap, bool, bool)> = Vec::new();
    let mut lines = Vec::new();
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
                clusters.push((
                    ClusterSnap {
                        glyphs: if newline(text, &t) { Vec::new() } else { ids },
                        advance,
                        text: t,
                    },
                    c.is_ligature_continuation(),
                    run.is_rtl(),
                ));
            }
        }
        let runs: f32 = line.runs().map(|r| r.advance()).sum();
        let emoji_sequence = text[line.text_range()]
            .graphemes(true)
            .any(|g| g.chars().count() > 1 && fonts::emoji_presentation(g));
        let n = glyphs.len();
        let emoji_defect =
            emoji_sequence && f64::from((runs - advance).abs()) <= bound(n + 2, advance.abs());
        lines.push(LineSnap {
            text: line.text_range(),
            // The line box top. (`block_min_coord` is the selection
            // box: with negative leading, Parley clamps it to ascent plus
            // descent; ours stays the line box.)
            top: m.baseline - m.ascent - m.leading * 0.5,
            height: m.line_height,
            baseline: m.baseline,
            advance: m.advance,
            trailing: m.trailing_whitespace,
            x: m.offset,
            glyphs,
            glyph_advance: advance,
            emoji_defect,
        });
    }
    // Clusters of one run split over lines arrive in line order; within a
    // line, `runs()` is visual order. Logical order is by text start.
    clusters.sort_by_key(|c| c.0.text.start);
    // Merges, in logical order: a ligature continuation joins its
    // ligature start (before it in a left-to-right run; after it in a
    // right-to-left one, where Parley lists the glyph-less continuation
    // first), uncounted; a cluster starting inside a grapheme joins the
    // one before (GB9c), counted.
    let mut merged: Vec<(ClusterSnap, bool, bool)> = Vec::with_capacity(clusters.len());
    let mut grapheme_merges = 0;
    for (c, cont, rtl) in clusters {
        let join = |last: &mut ClusterSnap, c: ClusterSnap| {
            last.text.end = c.text.end;
            last.glyphs.extend(c.glyphs);
            last.advance += c.advance;
        };
        match merged.last_mut() {
            Some((last, _, _))
                if cont && !rtl && last.text.end == c.text.start && !newline(text, &c.text) =>
            {
                join(last, c);
            }
            Some((last, lcont, true))
                if *lcont && last.glyphs.is_empty() && last.text.end == c.text.start =>
            {
                join(last, c);
                *lcont = cont;
            }
            Some((last, _, _))
                if grapheme(c.text.start) != c.text.start && last.text.end == c.text.start =>
            {
                grapheme_merges += 1;
                join(last, c);
            }
            _ => merged.push((c, cont, rtl)),
        }
    }
    let clusters: Vec<ClusterSnap> = merged.into_iter().map(|c| c.0).collect();
    Snapshot {
        grapheme_merges,
        rtl: layout.is_rtl(),
        width: layout.width(),
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
    /// Oracle lines compared by their glyph sum instead of their line
    /// advance: verified emoji-defect lines whose raw advance is off
    /// (`LineSnap::emoji_defect`). Tests assert the count per case.
    pub advance_fixups: usize,
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
                // The raw line advance, except on a verified emoji-defect
                // line whose raw advance is off: there, its glyph sum.
                let mut y_advance = y.advance;
                if y.emoji_defect
                    && f64::from((y.advance - y.glyph_advance).abs()) > bound(n + 2, s)
                {
                    d.advance_fixups += 1;
                    y_advance = y.glyph_advance;
                }
                d.check(Field::Metric, x.advance, y_advance, bound(n + 2, s));
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
        // With emoji-defect lines, the oracle width comes from the same
        // corrected advances.
        let n = a.lines.iter().map(|l| l.glyphs.len()).max().unwrap_or(0);
        let width = if d.advance_fixups == 0 {
            b.width
        } else {
            b.lines
                .iter()
                .map(|l| if l.emoji_defect { l.glyph_advance } else { l.advance } - l.trailing)
                .fold(0.0, f32::max)
        };
        d.check(Field::Metric, a.width, width, bound(n + 3, a.width));
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
        // Span zero's line height is the paragraph's.
        if let Some(lh) = spans
            .first()
            .map(|s| s.style.line_height)
            .filter(|&h| h > 0.0)
        {
            b.push_default(StyleProperty::LineHeight(parley::LineHeight::Absolute(lh)));
        }
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
            if s.style.letter_spacing != 0.0 {
                b.push(
                    StyleProperty::LetterSpacing(s.style.letter_spacing),
                    r.clone(),
                );
            }
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
    let spec = TextSpec { text, spans };
    engine.layout_text(&spec, width)
}

// --- MARK: editing (E01 editable cases) ---

use craie_text::editor::{Editor, Motion};
use parley::PlainEditor;

/// One editing step, applied the same way to both editors.
#[derive(Clone, Debug)]
pub enum Op {
    Insert(&'static str),
    Backspace,
    Delete,
    BackspaceWord,
    DeleteWord,
    Move(Motion),
    Select(Motion),
    SelectAll,
    Point(f32, f32),
    Extend(f32, f32),
    WordAt(f32, f32),
    Range(u32, u32),
    Compose(&'static str, Option<(u32, u32)>),
    ClearCompose,
    FinishCompose,
}

/// What both editors must agree on after a step: the buffer (preedit
/// included), the selection's anchor and focus bytes, and the preedit
/// range.
#[derive(Clone, Debug, PartialEq)]
pub struct EditState {
    pub text: String,
    pub anchor: usize,
    pub focus: usize,
    pub compose: Option<Range<usize>>,
}

/// The owned editor on the pinned fonts.
pub fn editor(text: &str, size: f32, width: Option<f32>, engine: &mut TextEngine) -> Editor {
    let mut ed = Editor::new(size);
    ed.set_text(text);
    ed.set_width(width);
    ed.refresh(engine);
    ed
}

pub fn apply_ours(ed: &mut Editor, engine: &mut TextEngine, op: &Op) {
    ed.refresh(engine);
    match *op {
        Op::Insert(s) => ed.insert_or_replace_selection(engine, s),
        Op::Backspace => ed.backdelete(engine),
        Op::Delete => ed.delete(engine),
        Op::BackspaceWord => ed.backdelete_word(engine),
        Op::DeleteWord => ed.delete_word(engine),
        Op::Move(m) => ed.motion(m, false),
        Op::Select(m) => ed.motion(m, true),
        Op::SelectAll => ed.select_all(),
        Op::Point(x, y) => ed.move_to_point(x, y),
        Op::Extend(x, y) => ed.extend_to_point(x, y),
        Op::WordAt(x, y) => ed.select_word_at_point(x, y),
        Op::Range(a, b) => ed.select_byte_range(a, b),
        Op::Compose(s, c) => ed.set_compose(engine, s, c),
        Op::ClearCompose => ed.clear_compose(engine),
        Op::FinishCompose => ed.finish_compose(engine),
    }
    ed.refresh(engine);
}

pub fn state_ours(ed: &Editor) -> EditState {
    let s = ed.selection();
    EditState {
        text: ed.raw_text().to_string(),
        anchor: s.anchor.index as usize,
        focus: s.focus.index as usize,
        compose: ed.raw_compose().map(|r| r.start as usize..r.end as usize),
    }
}

impl Oracle {
    /// Parley's editor over the pinned fonts, unquantized as E01 lays
    /// out.
    pub fn editor(&mut self, text: &str, size: f32, width: Option<f32>) -> PlainEditor<u16> {
        let mut ed = PlainEditor::new(size);
        ed.edit_styles()
            .insert(StyleProperty::FontFamily(FontFamily::Source(
                Cow::Borrowed(STACK),
            )));
        ed.set_quantize(false);
        ed.set_text(text);
        ed.set_width(width);
        ed.refresh_layout(&mut self.font_cx, &mut self.layout_cx);
        ed
    }

    /// Brings Parley's editor layout up to date (a width change relays
    /// it out).
    pub fn refresh(&mut self, ed: &mut PlainEditor<u16>) {
        ed.refresh_layout(&mut self.font_cx, &mut self.layout_cx);
    }

    pub fn apply(&mut self, ed: &mut PlainEditor<u16>, op: &Op) {
        let mut d = ed.driver(&mut self.font_cx, &mut self.layout_cx);
        let motion =
            |d: &mut parley::PlainEditorDriver<'_, u16>, m: Motion, extend: bool| match (m, extend)
            {
                (Motion::Left, false) => d.move_left(),
                (Motion::Right, false) => d.move_right(),
                (Motion::WordLeft, false) => d.move_word_left(),
                (Motion::WordRight, false) => d.move_word_right(),
                (Motion::LineStart, false) => d.move_to_line_start(),
                (Motion::LineEnd, false) => d.move_to_line_end(),
                (Motion::Up, false) => d.move_up(),
                (Motion::Down, false) => d.move_down(),
                (Motion::TextStart, false) => d.move_to_text_start(),
                (Motion::TextEnd, false) => d.move_to_text_end(),
                (Motion::Left, true) => d.select_left(),
                (Motion::Right, true) => d.select_right(),
                (Motion::WordLeft, true) => d.select_word_left(),
                (Motion::WordRight, true) => d.select_word_right(),
                (Motion::LineStart, true) => d.select_to_line_start(),
                (Motion::LineEnd, true) => d.select_to_line_end(),
                (Motion::Up, true) => d.select_up(),
                (Motion::Down, true) => d.select_down(),
                (Motion::TextStart, true) => d.select_to_text_start(),
                (Motion::TextEnd, true) => d.select_to_text_end(),
            };
        match *op {
            Op::Insert(s) => d.insert_or_replace_selection(s),
            Op::Backspace => d.backdelete(),
            Op::Delete => d.delete(),
            Op::BackspaceWord => d.backdelete_word(),
            Op::DeleteWord => d.delete_word(),
            Op::Move(m) => motion(&mut d, m, false),
            Op::Select(m) => motion(&mut d, m, true),
            Op::SelectAll => d.select_all(),
            Op::Point(x, y) => d.move_to_point(x, y),
            Op::Extend(x, y) => d.extend_selection_to_point(x, y),
            Op::WordAt(x, y) => d.select_word_at_point(x, y),
            Op::Range(a, b) => d.select_byte_range(a as usize, b as usize),
            Op::Compose(s, c) => d.set_compose(s, c.map(|(a, b)| (a as usize, b as usize))),
            Op::ClearCompose => d.clear_compose(),
            Op::FinishCompose => d.finish_compose(),
        }
        d.refresh_layout();
    }
}

pub fn state_parley(ed: &PlainEditor<u16>) -> EditState {
    let s = ed.raw_selection();
    EditState {
        text: ed.raw_text().to_string(),
        anchor: s.anchor().index(),
        focus: s.focus().index(),
        compose: ed.raw_compose().clone(),
    }
}
