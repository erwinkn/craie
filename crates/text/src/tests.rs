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
    assert!(xs.windows(2).all(|w| w[0] <= w[1] + 1e-3), "{xs:?}");
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
    assert!(heb.x > p.segs[0].x);
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
