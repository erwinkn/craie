//! Owned paragraph tests on the pinned fonts.

use crate::TextEngine;
use crate::fonts::pinned;
use crate::paragraph::{HIDDEN, Paragraph, SpanStyle, TextSpec, TextStyle};

fn engine() -> TextEngine {
    TextEngine::with_source(Box::new(pinned()))
}

fn layout(e: &mut TextEngine, text: &str, width: Option<f32>) -> Paragraph {
    let spans = [SpanStyle {
        start: 0,
        style: TextStyle {
            size: 16.0,
            ..TextStyle::default()
        },
    }];
    e.layout_text(
        &TextSpec {
            text,
            family: "sans-serif",
            spans: &spans,
        },
        width,
    )
}

/// Lines cover the text in order, without gaps.
fn check_lines(p: &Paragraph, text: &str) {
    let mut at = 0;
    for l in &p.lines {
        assert_eq!(l.text.start, at, "lines are contiguous");
        at = l.text.end;
    }
    assert_eq!(at as usize, text.len());
}

#[test]
fn latin_wraps_at_word_boundaries() {
    let mut e = engine();
    let text = "The quick brown fox jumps over the lazy dog again and again";
    let p = layout(&mut e, text, Some(120.0));
    assert!(p.lines.len() > 2, "{} lines", p.lines.len());
    check_lines(&p, text);
    for l in &p.lines {
        // Breaks fall before a word: every line but the first starts at a
        // letter after a space.
        if l.text.start > 0 {
            assert_eq!(&text[l.text.start as usize - 1..l.text.start as usize], " ");
        }
        assert!(l.advance - l.trailing <= 120.0 + 1e-3);
    }
    let sum: f32 = p.lines.iter().map(|l| l.height).sum();
    assert!((p.height - sum).abs() < 1e-3);
    assert!(p.glyphs.iter().all(|g| g.id != 0), "no .notdef");
    // Unbounded: one line as wide as the text.
    let one = layout(&mut e, text, None);
    assert_eq!(one.lines.len(), 1);
    assert!(one.width > 300.0);
}

#[test]
fn newlines_end_lines() {
    let mut e = engine();
    let p = layout(&mut e, "one\ntwo\n", None);
    assert_eq!(p.lines.len(), 3, "a trailing newline leaves an empty line");
    let h = p.lines[0].height;
    assert!(
        (p.height - 2.0 * h).abs() < 1e-3,
        "the empty last line adds no height"
    );
    assert_eq!(p.lines[0].caret_end, 3);
    assert!(p.glyphs.iter().any(|g| g.id == HIDDEN));
    // The caret after "one" sits at the line's end, not past it.
    let c = e_caret(&p, 3);
    let hit = p.hit(c + 50.0, p.lines[0].top + 1.0);
    assert_eq!(hit.offset, 3);
}

fn e_caret(p: &Paragraph, offset: u32) -> f32 {
    p.caret(offset).x
}

#[test]
fn rtl_paragraph_aligns_right_and_orders_visually() {
    let mut e = engine();
    let text = "שלום עולם";
    let p = layout(&mut e, text, Some(300.0));
    assert!(p.base_rtl);
    let line = &p.lines[0];
    let right = line.x + line.advance - line.trailing;
    assert!((right - 300.0).abs() < 1e-2, "right aligned: {right}");
    // Visual order (the line's segments, left to right): clusters
    // decrease. The space is its own run (the primary face covers it).
    let clusters: Vec<u32> = p.segs[line.segs.start as usize..line.segs.end as usize]
        .iter()
        .flat_map(|s| {
            p.glyphs[s.glyphs.start as usize..s.glyphs.end as usize]
                .iter()
                .map(|g| g.cluster)
        })
        .collect();
    assert!(clusters.windows(2).all(|w| w[0] >= w[1]), "{clusters:?}");
    // Glyph x increases left to right along the segments.
    let xs: Vec<f32> = p
        .segs
        .iter()
        .flat_map(|s| {
            p.glyphs[s.glyphs.start as usize..s.glyphs.end as usize]
                .iter()
                .map(|g| g.x)
        })
        .collect();
    assert!(xs.windows(2).all(|w| w[0] <= w[1]), "{xs:?}");
}

#[test]
fn mixed_bidi_reorders_segments() {
    let mut e = engine();
    let text = "abc אבג def";
    let p = layout(&mut e, text, None);
    assert!(!p.base_rtl);
    let segs: Vec<&str> = p
        .segs
        .iter()
        .map(|s| &text[s.text.start as usize..s.text.end as usize])
        .collect();
    // LTR base: "abc ", the Hebrew run, " def" left to right.
    assert!(segs.first().unwrap().starts_with("abc"), "{segs:?}");
    assert!(segs.last().unwrap().ends_with("def"), "{segs:?}");
    let heb = p
        .segs
        .iter()
        .find(|s| p.runs[s.run as usize].rtl())
        .unwrap();
    let x = |seg: &crate::paragraph::Seg| p.glyphs[seg.glyphs.start as usize].x;
    assert!(x(heb) > x(&p.segs[0]));
}

#[test]
fn fallback_covers_the_todo_cross() {
    let mut e = engine();
    let text = "a ✕ b";
    let p = layout(&mut e, text, None);
    let cross = text.find('✕').unwrap() as u32;
    let g = p.glyphs.iter().find(|g| g.cluster == cross).unwrap();
    assert_ne!(g.id, 0, "✕ has a glyph (not .notdef)");
    let fonts: std::collections::BTreeSet<_> = p.runs.iter().map(|r| r.font).collect();
    assert!(fonts.len() >= 2, "a fallback font serves ✕");
    // Arabic, Devanagari, and Japanese resolve too.
    for s in ["مرحبا بالعالم", "नमस्ते दुनिया", "日本語の文章"]
    {
        let p = layout(&mut e, s, None);
        assert!(p.glyphs.iter().all(|g| g.id != 0), "{s}: no .notdef");
    }
}

#[test]
fn caret_and_hit_round_trip() {
    let mut e = engine();
    for text in ["Hello world, again", "abc אבג def", "مرحبا بالعالم"] {
        let p = layout(&mut e, text, Some(90.0));
        for (i, _) in text.char_indices() {
            let i = i as u32;
            // Offsets inside a cluster have no caret of their own.
            if !p.glyphs.iter().any(|g| g.cluster == i) {
                continue;
            }
            let c = p.caret(i);
            let rtl = p.runs.iter().any(|r| r.rtl() && r.text.contains(&i));
            // Just inside the cluster on its leading side.
            let x = if rtl { c.x - 0.5 } else { c.x + 0.5 };
            let hit = p.hit(x, c.top + c.height * 0.5);
            assert_eq!(hit.offset, i, "{text:?}: offset {i} caret {c:?}");
        }
    }
}

#[test]
fn selection_covers_the_range() {
    let mut e = engine();
    let text = "Hello world, again and again";
    let p = layout(&mut e, text, Some(90.0));
    let rects = p.selection_rects(0..text.len() as u32);
    assert_eq!(rects.len(), p.lines.len(), "one rect per line");
    let word = p.selection_rects(6..11);
    assert_eq!(word.len(), 1);
    let (x, _, w, _) = word[0];
    assert!((x - p.caret(6).x).abs() < 1e-3 && (x + w - p.caret(11).x).abs() < 1e-3);
}

#[test]
fn rewrap_does_not_shape() {
    let mut e = engine();
    let text = "wrap me at several widths please";
    let mut p = layout(&mut e, text, Some(400.0));
    let shapes = e.shapes;
    let glyphs = p
        .glyphs
        .iter()
        .map(|g| (g.id, g.cluster))
        .collect::<Vec<_>>();
    e.rewrap(&mut p, Some(60.0));
    assert_eq!(e.shapes, shapes);
    assert!(p.lines.len() > 1);
    check_lines(&p, text);
    assert_eq!(
        glyphs,
        p.glyphs
            .iter()
            .map(|g| (g.id, g.cluster))
            .collect::<Vec<_>>()
    );
}

#[test]
fn empty_text_has_a_caret() {
    let mut e = engine();
    let p = layout(&mut e, "", Some(100.0));
    assert_eq!(p.height, 0.0);
    let c = p.caret(0);
    assert!(c.height > 10.0);
}

/// Each line's visual character order equals unicode-bidi's L1 + L2
/// reordering of that line (the reference for L1, which Parley does not
/// apply): trailing whitespace, tabs, and whitespace before them take the
/// paragraph level; numbers and nested runs reverse by L2.
#[test]
fn line_order_matches_unicode_bidi() {
    use unicode_bidi::BidiInfo;
    let mut e = engine();
    let texts = [
        "Hello שלום world مرحبا بالعالم 123 end.",
        "مرحبا بالعالم with English inside and ٣٤٥ numbers، ثم نهاية الجملة.",
        "abc\tשלום עולם\tdef 12 34",
        "שלום\tabc  עולם   \nsecond שורה with 3.14 and ١٢٣  ",
        "(مرحبا [world] 42%) end",
    ];
    for text in texts {
        let bidi = BidiInfo::new(text, None);
        for width in [None, Some(40.0), Some(90.0), Some(160.0)] {
            let p = layout(&mut e, text, width);
            for line in &p.lines {
                let (a, b) = (line.text.start as usize, line.text.end as usize);
                if a == b {
                    continue;
                }
                let para = bidi
                    .paragraphs
                    .iter()
                    .find(|q| q.range.contains(&a))
                    .unwrap();
                // The reference: L1 + L2 over the line's characters,
                // without the paragraph separator.
                let end = b.min(para.range.end);
                let levels = bidi.reordered_levels_per_char(para, a..end);
                let chars: Vec<(usize, char)> = text.char_indices().collect();
                let line_chars: Vec<usize> = (0..chars.len())
                    .filter(|&k| chars[k].0 >= a && chars[k].0 < end)
                    .collect();
                let line_levels: Vec<_> = line_chars.iter().map(|&k| levels[k]).collect();
                let want: Vec<char> = BidiInfo::reorder_visual(&line_levels)
                    .into_iter()
                    .map(|v| chars[line_chars[v]].1)
                    .filter(|c| !super::paragraph::is_newline(*c))
                    .collect();
                // Ours: segments in order, a segment's characters reversed
                // at an odd level.
                let mut got: Vec<char> = Vec::new();
                for s in &p.segs[line.segs.start as usize..line.segs.end as usize] {
                    let t = &text[s.text.start as usize..s.text.end as usize];
                    let cs = t.chars().filter(|c| !super::paragraph::is_newline(*c));
                    if s.level & 1 == 1 {
                        got.extend(cs.rev());
                    } else {
                        got.extend(cs);
                    }
                }
                assert_eq!(
                    got.iter().collect::<String>(),
                    want.iter().collect::<String>(),
                    "{text:?} at {width:?}, line {a}..{b}"
                );
            }
        }
    }
}

/// The bytes a laid-out paragraph's runs use, per run.
fn run_bytes(e: &TextEngine, p: &Paragraph) -> Vec<usize> {
    p.runs
        .iter()
        .map(|r| {
            let store = &e.fonts.store;
            let face = store.instance_data(r.font).face;
            store.face_data(face).bytes.as_ref().as_ref().as_ptr() as usize
        })
        .collect()
}

fn reversed_pinned() -> crate::fonts::RawFonts {
    let mut fonts = crate::fonts::RawFonts::new();
    for bytes in crate::fonts::pinned_files().into_iter().rev() {
        fonts.add_static(bytes);
    }
    fonts
}

/// A new source never reaches the old source's faces through a reused
/// byte identity: after `set_source` with the files registered in
/// reverse order, layout equals a fresh engine on that source.
#[test]
fn source_replacement_keeps_byte_identity() {
    let text = "abc שלום ✕ नमस्ते";
    let mut e = engine();
    let before = layout(&mut e, text, None);
    e.fonts.set_source(Box::new(reversed_pinned()));
    let after = layout(&mut e, text, None);
    let mut fresh = TextEngine::with_source(Box::new(reversed_pinned()));
    let want = layout(&mut fresh, text, None);
    let ids = |p: &Paragraph| p.glyphs.iter().map(|g| g.id).collect::<Vec<_>>();
    assert_eq!(ids(&after), ids(&want));
    assert_eq!(run_bytes(&e, &after), run_bytes(&fresh, &want));
    assert_eq!(ids(&before), ids(&layout(&mut engine(), text, None)));
}

/// A profile whose primary (the JP subset) covers none of the test
/// clusters, so every one goes to fallback. Registration order is the
/// fallback order: Hebrew, Symbols 2, Arabic, Devanagari, Sans.
fn fallback_profile() -> crate::fonts::RawFonts {
    let f = crate::fonts::pinned_files();
    let mut fonts = crate::fonts::RawFonts::new();
    for k in [6, 4, 7, 3, 5, 0] {
        fonts.add_static(f[k]);
    }
    fonts.set_default_family("Noto Sans JP");
    fonts
}

/// Fallback depends on the cluster alone: each text laid out after the
/// others, in either order, equals a fresh engine's layout. The cases:
/// "1" then "1" + shadda (Symbols 2 covers the digit, only Arabic covers
/// both), and "1◌" then "◌" (Symbols 2 covers both, but Hebrew comes
/// first in the source order for ◌).
#[test]
fn fallback_depends_on_the_cluster_alone() {
    let texts = [
        "1",
        "1\u{0651}",
        "1\u{20E3}",
        "1\u{25CC}",
        "\u{25CC}",
        "\u{25CC}\u{05B0}",
    ];
    let fresh: Vec<(Vec<u16>, Vec<usize>)> = texts
        .iter()
        .map(|t| {
            let mut e = TextEngine::with_source(Box::new(fallback_profile()));
            let p = layout(&mut e, t, None);
            (p.glyphs.iter().map(|g| g.id).collect(), run_bytes(&e, &p))
        })
        .collect();
    // No .notdef: some candidate covers each whole cluster.
    for (t, (ids, _)) in texts.iter().zip(&fresh) {
        assert!(ids.iter().all(|&g| g != 0), "{t:?}: {ids:?}");
    }
    let f = crate::fonts::pinned_files();
    let ptr = |k: usize| f[k].as_ptr() as usize;
    assert_eq!(fresh[1].1, [ptr(3)], "digit + shadda: Arabic");
    assert_eq!(fresh[4].1, [ptr(4)], "◌: Hebrew, first in source order");
    for order in [false, true] {
        let mut e = TextEngine::with_source(Box::new(fallback_profile()));
        let mut idx: Vec<usize> = (0..texts.len()).collect();
        if order {
            idx.reverse();
        }
        for k in idx {
            let p = layout(&mut e, texts[k], None);
            let got = (
                p.glyphs.iter().map(|g| g.id).collect::<Vec<_>>(),
                run_bytes(&e, &p),
            );
            assert_eq!(got, fresh[k], "{:?} (reverse: {order})", texts[k]);
        }
    }
}

/// Each hard-break form ends exactly one line; CRLF is one break (UAX #14
/// LB5), and a caret never falls between CR and LF.
#[test]
fn hard_breaks_end_one_line_each() {
    let mut e = engine();
    for (text, lines) in [
        ("one\r\ntwo", 2),
        ("one\rtwo", 2),
        ("one\ntwo", 2),
        ("one\u{85}two", 2),
        ("one\u{2028}two", 2),
        ("one\u{2029}two", 2),
        ("a\r\n\r\nb", 3),
        ("end\r\n", 2),
    ] {
        let p = layout(&mut e, text, None);
        assert_eq!(p.lines.len(), lines, "{text:?}");
        check_lines(&p, text);
        let cr = text.find('\r');
        if let Some(cr) = cr.filter(|&i| text[i..].starts_with("\r\n")) {
            assert!(
                p.cluster_map()
                    .iter()
                    .any(|(t, _)| *t == (cr as u32..cr as u32 + 2))
            );
        }
    }
}

/// No-break spaces (NBSP, narrow NBSP, figure space) never break: the
/// protected phrase overflows at every width, zero included.
#[test]
fn no_break_spaces_do_not_break() {
    let mut e = engine();
    for nb in ['\u{00A0}', '\u{202F}', '\u{2007}'] {
        let text = format!("a{nb}b{nb}c");
        for w in [0.0, 5.0, 12.0, 20.0] {
            let p = layout(&mut e, &text, Some(w));
            assert_eq!(p.lines.len(), 1, "{text:?} at {w}");
        }
        // A plain space still breaks.
        let p = layout(&mut e, &text.replace(nb, " "), Some(5.0));
        assert_eq!(p.lines.len(), 3);
    }
}

/// Over a mixed corpus at many widths: every line after the first starts
/// at a UAX #14 opportunity, and every mandatory break starts a line.
#[test]
fn lines_start_at_break_opportunities() {
    use unicode_linebreak::{BreakOpportunity, linebreaks};
    let mut e = engine();
    let corpus = [
        "The quick brown fox (jumps) over — the lazy dog! Is it 3.14 or 2,718?",
        "a\u{00A0}b c\u{202F}d e !f \"quoted text\" end.\r\nnext line\rthird\u{2028}fourth",
        "Hello שלום world مرحبا بالعالم 123 end.\nנמסטה नमस्ते दुनिया",
        "日本語のテキストを表示します。改行、組版、段落。",
    ];
    for text in corpus {
        let ops: Vec<(usize, BreakOpportunity)> = linebreaks(text).collect();
        for w in [0.0, 10.0, 30.0, 55.0, 80.0, 120.0, 200.0, 400.0] {
            let p = layout(&mut e, text, Some(w));
            check_lines(&p, text);
            let starts: Vec<usize> = p.lines.iter().map(|l| l.text.start as usize).collect();
            for &s in &starts[1..] {
                assert!(
                    ops.iter().any(|&(i, _)| i == s),
                    "{text:?} at {w}: break at {s}"
                );
            }
            for &(i, op) in &ops {
                if op == BreakOpportunity::Mandatory && i < text.len() {
                    assert!(
                        starts.contains(&i),
                        "{text:?} at {w}: no line at mandatory {i}"
                    );
                }
            }
        }
    }
}

/// Cluster edges from the placements alone: the smallest glyph x of a
/// cluster and the largest glyph x + advance (an oracle independent of
/// the mapping code's traversal).
fn placed_edges(p: &Paragraph, glyphs: std::ops::Range<u32>) -> (f32, f32) {
    let g = &p.glyphs[glyphs.start as usize..glyphs.end as usize];
    let left = g.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
    let right = g
        .iter()
        .map(|g| g.x + g.advance)
        .fold(f32::NEG_INFINITY, f32::max);
    (left, right)
}

/// Carets, selection rectangles, and hit tests agree exactly with the
/// placements, on multi-glyph clusters (Devanagari conjuncts, Arabic with
/// marks, Latin with stacked marks) and bidi text, at several widths;
/// and along each segment, cluster starts rise left to right at an even
/// level and fall at an odd one, as drawn.
#[test]
fn mapping_reads_the_placements() {
    let mut e = engine();
    let texts = [
        "क्षत्रिय स्त्री द्वार हिन्दी",
        "مَرْحَبًا بِالْعَالَمِ",
        "a\u{0301}\u{0323} e\u{0308}\u{0304} q\u{0307}\u{0323}\u{0301}",
        "abc אבג \u{2002}\u{2003}דהו xyz",
        "שלום abc \u{2002}\u{2003}def עולם",
    ];
    let mut multi = 0;
    let mut reversed = 0;
    for text in texts {
        for w in [None, Some(30.0), Some(60.0), Some(90.0), Some(140.0)] {
            let p = layout(&mut e, text, w);
            for line in &p.lines {
                for seg in &p.segs[line.segs.start as usize..line.segs.end as usize] {
                    let rtl = seg.level & 1 == 1;
                    if (seg.level ^ p.runs[seg.run as usize].level) & 1 == 1 {
                        reversed += 1;
                    }
                    // Clusters of this segment, by their placed left edge.
                    let mut cl: Vec<(std::ops::Range<u32>, std::ops::Range<u32>)> = p
                        .cluster_map()
                        .into_iter()
                        .filter(|(_, g)| g.start >= seg.glyphs.start && g.end <= seg.glyphs.end)
                        .collect();
                    cl.sort_by(|a, b| {
                        placed_edges(&p, a.1.clone())
                            .0
                            .total_cmp(&placed_edges(&p, b.1.clone()).0)
                    });
                    let starts: Vec<u32> = cl.iter().map(|c| c.0.start).collect();
                    assert!(
                        starts.windows(2).all(|w| (w[0] < w[1]) != rtl),
                        "{text:?} at {w:?}: cluster order {starts:?} (level {})",
                        seg.level
                    );
                    for (t, g) in &cl {
                        if g.len() > 1 {
                            multi += 1;
                        }
                        let (left, right) = placed_edges(&p, g.clone());
                        let caret = p.caret(t.start);
                        assert_eq!(
                            caret.x,
                            if rtl { right } else { left },
                            "{text:?} caret at {}",
                            t.start
                        );
                        assert_eq!(
                            p.selection_rects(t.clone()),
                            [(left, line.top, right - left, line.height)],
                            "{text:?} selection {t:?}"
                        );
                        if right == left {
                            // A newline: no width to hit.
                            continue;
                        }
                        let y = line.top + line.height * 0.5;
                        let near_left = p.hit(left + (right - left) * 0.25, y);
                        let near_right = p.hit(left + (right - left) * 0.75, y);
                        let (a, b) = if rtl {
                            (t.end, t.start)
                        } else {
                            (t.start, t.end)
                        };
                        assert_eq!(
                            (near_left.offset, near_right.offset),
                            (a, b),
                            "{text:?} hit {t:?}"
                        );
                    }
                }
            }
        }
    }
    assert!(multi > 10, "multi-glyph clusters checked: {multi}");
    assert!(reversed > 0, "no L1-reversed segment was checked");
}
