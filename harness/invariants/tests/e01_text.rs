//! E01 correctness: the owned paragraph against Parley on the pinned
//! fonts, per case and width. Line breaks, cluster mappings (text range
//! and glyph ids per group), byte-to-cluster queries, and drawn glyphs
//! (id, source cluster, font file) must match exactly, except where
//! UAX #9 rule L1 alone separates the two (`e01::Diff`) and where Parley
//! splits a grapheme (merged and counted); advances bit-equal; positions
//! and line metrics within the f32 summation bound of `e01::compare`.

use craie_harness::e01::{self, Oracle};

#[test]
fn owned_paragraph_matches_parley() {
    let mut engine = e01::engine();
    let mut oracle = Oracle::new();
    let mut failures = Vec::new();
    for case in e01::cases() {
        for &w in &case.widths {
            let p = e01::layout(&mut engine, &case.text, &case.spans, w);
            // The pinned fonts cover every case but the missing-glyph one.
            let notdef = p.glyphs.iter().any(|g| g.id == 0);
            assert_eq!(notdef, case.notdef, "{}: .notdef", case.name);
            let l = oracle.layout(&case.text, &case.spans, w);
            let theirs = e01::parley(&l, &case.text);
            let d = e01::compare(&e01::ours(&engine, &p, &case.text), &theirs, &case.text);
            eprintln!(
                "{:14} {:13} w={:>5?}: breaks {} clusters {} drawn {} l1 {} graph {} advfix {} adv {:.2e} pos {:.2e} metrics {:.2e} worst {:.3}",
                case.name,
                case.class,
                w,
                d.line_breaks,
                d.clusters,
                d.glyph_ids,
                d.l1_lines,
                theirs.grapheme_merges,
                theirs.advance_fixups,
                d.advance,
                d.position,
                d.metrics,
                d.worst
            );
            if !d.pass() {
                failures.push(format!("{} w={w:?}: {d:?}", case.name));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A width change rewraps the shaped runs: the result equals a fresh
/// layout at the new width, from every start width.
#[test]
fn rewrap_equals_fresh_layout() {
    let mut engine = e01::engine();
    for case in e01::cases() {
        for &from in &case.widths {
            let mut p = e01::layout(&mut engine, &case.text, &case.spans, from);
            for &to in &case.widths {
                engine.rewrap(&mut p, to);
                let fresh = e01::layout(&mut engine, &case.text, &case.spans, to);
                assert_eq!(
                    e01::ours(&engine, &p, &case.text),
                    e01::ours(&engine, &fresh, &case.text),
                    "{} {from:?} -> {to:?}",
                    case.name
                );
            }
        }
    }
}

/// The comparison finds each kind of difference: a moved line break, a
/// changed glyph, swapped visible glyphs (which L1 must not excuse), and
/// a glyph moved by 0.01 pt.
#[test]
fn comparison_detects_differences() {
    let mut engine = e01::engine();
    let mut oracle = Oracle::new();
    let cases = e01::cases();
    let case = cases.iter().find(|c| c.name == "bidi-ltr").unwrap();
    let w = Some(90.0);
    let p = e01::layout(&mut engine, &case.text, &case.spans, w);
    let a = e01::ours(&engine, &p, &case.text);
    let b = e01::parley(&oracle.layout(&case.text, &case.spans, w), &case.text);
    assert!(e01::compare(&a, &b, &case.text).pass());

    let mut m = a.clone();
    m.lines[0].text.end -= 1;
    assert_eq!(e01::compare(&m, &b, &case.text).line_breaks, 1);

    let mut m = a.clone();
    m.clusters[3].glyphs[0] += 1;
    assert_eq!(e01::compare(&m, &b, &case.text).clusters, 1);

    let mut m = a.clone();
    m.lines[1].glyphs.swap(0, 1);
    let d = e01::compare(&m, &b, &case.text);
    assert_eq!((d.glyph_ids, d.pass()), (1, false));

    // A drawn glyph's source cluster or font file alone.
    let mut m = a.clone();
    m.lines[2].glyphs[1].cluster += 1;
    assert_eq!(e01::compare(&m, &b, &case.text).glyph_ids, 1);
    let mut m = a.clone();
    m.lines[2].glyphs[1].font ^= 1;
    assert_eq!(e01::compare(&m, &b, &case.text).glyph_ids, 1);

    let mut m = a.clone();
    m.lines[2].glyphs[1].x += 0.01;
    let d = e01::compare(&m, &b, &case.text);
    assert!(d.exact_structure() && !d.pass(), "{d:?}");
}

/// Byte-to-cluster queries against Parley's `Cluster::from_byte_index`,
/// at every byte (inside multibyte characters too): ours is the merged
/// oracle cluster that holds the byte, and Parley's own cluster lies
/// inside it. Cluster-to-byte: each cluster's first and last byte map
/// back to it.
#[test]
fn byte_to_cluster_matches_parley() {
    let mut engine = e01::engine();
    let mut oracle = Oracle::new();
    for case in e01::cases() {
        let text = &case.text;
        let p = e01::layout(&mut engine, text, &case.spans, None);
        let l = oracle.layout(text, &case.spans, None);
        let merged = e01::parley(&l, text).clusters;
        for b in 0..text.len() {
            let ours = p
                .cluster_at(b as u32)
                .map(|r| r.start as usize..r.end as usize);
            let want = merged
                .iter()
                .find(|c| c.text.contains(&b))
                .map(|c| c.text.clone());
            assert_eq!(ours, want, "{} byte {b}", case.name);
            let theirs = parley::Cluster::from_byte_index(&l, b).map(|c| c.text_range());
            let (want, theirs) = (want.unwrap(), theirs.unwrap());
            assert!(
                want.start <= theirs.start && theirs.end <= want.end,
                "{} byte {b}: {theirs:?} outside {want:?}",
                case.name
            );
        }
        for (t, _) in p.cluster_map() {
            assert_eq!(p.cluster_at(t.start), Some(t.clone()), "{}", case.name);
            assert_eq!(p.cluster_at(t.end - 1), Some(t.clone()), "{}", case.name);
        }
        assert_eq!(p.cluster_at(text.len() as u32), None);
    }
}

/// Hard breaks where Parley differs from UAX #14: Parley breaks after CR
/// and again after LF in a CRLF (an extra line holding the LF), and
/// does not break after NEL. Ours follows UAX #14 (LB5): its line starts
/// are exactly the mandatory breaks; Parley's are ours plus each CRLF's
/// LF and minus each NEL's successor.
#[test]
fn hard_breaks_follow_uax14_where_parley_differs() {
    use unicode_linebreak::{BreakOpportunity, linebreaks};
    let text = "one\r\ntwo\rthree\nfour\u{85}five\u{2028}six\u{2029}seven\r\n\r\nend";
    let spans = [craie_text::paragraph::SpanStyle {
        start: 0,
        style: craie_text::paragraph::TextStyle {
            size: 15.0,
            weight: 400,
            italic: false,
        },
    }];
    let mut engine = e01::engine();
    let p = e01::layout(&mut engine, text, &spans, None);
    let ours: Vec<usize> = p.lines.iter().map(|l| l.text.start as usize).collect();
    let mut uax14: Vec<usize> = vec![0];
    uax14.extend(
        linebreaks(text)
            .filter(|&(i, op)| op == BreakOpportunity::Mandatory && i < text.len())
            .map(|(i, _)| i),
    );
    assert_eq!(ours, uax14);
    let mut want: Vec<usize> = ours.clone();
    want.extend(text.match_indices("\r\n").map(|(i, _)| i + 1));
    want.retain(|&i| !text[..i].ends_with('\u{85}'));
    want.sort_unstable();
    let l = Oracle::new().layout(text, &spans, None);
    let theirs: Vec<usize> = l.lines().map(|l| l.text_range().start).collect();
    assert_eq!(theirs, want);
}

/// A grapheme keeps one font: for a dotted circle with a Hebrew point,
/// which the primary (Noto Sans) covers only in part, ours draws both
/// from Noto Sans Hebrew, which covers both; Parley draws the base from
/// Noto Sans and the mark from Noto Sans Hebrew.
#[test]
fn grapheme_keeps_one_font_where_parley_splits() {
    let text = "x\u{25CC}\u{05B0}";
    let spans = [craie_text::paragraph::SpanStyle {
        start: 0,
        style: craie_text::paragraph::TextStyle {
            size: 16.0,
            weight: 400,
            italic: false,
        },
    }];
    let mut engine = e01::engine();
    let p = e01::layout(&mut engine, text, &spans, None);
    let fonts = |s: &e01::Snapshot| -> Vec<(usize, usize)> {
        s.lines[0]
            .glyphs
            .iter()
            .map(|g| (g.cluster, g.font))
            .collect()
    };
    // Pinned file 0 is Noto Sans, 4 Noto Sans Hebrew.
    assert_eq!(
        fonts(&e01::ours(&engine, &p, text)),
        [(0, 0), (1, 4), (1, 4)]
    );
    let l = Oracle::new().layout(text, &spans, None);
    assert_eq!(fonts(&e01::parley(&l, text)), [(0, 0), (1, 0), (1, 4)]);
}

/// No-break spaces where Parley differs from UAX #14 (LB12): at width 0,
/// Parley breaks after NBSP, narrow NBSP, and figure space; ours keeps
/// each protected phrase on one line, and every line start is a UAX #14
/// opportunity.
#[test]
fn no_break_spaces_follow_uax14_where_parley_differs() {
    use unicode_linebreak::linebreaks;
    let text = "a\u{00A0}b c\u{202F}d e\u{2007}f end";
    let spans = [craie_text::paragraph::SpanStyle {
        start: 0,
        style: craie_text::paragraph::TextStyle {
            size: 15.0,
            weight: 400,
            italic: false,
        },
    }];
    let allowed: Vec<usize> = linebreaks(text).map(|(i, _)| i).collect();
    let mut engine = e01::engine();
    let mut oracle = Oracle::new();
    for w in [0.0, 5.0, 12.0] {
        let p = e01::layout(&mut engine, text, &spans, Some(w));
        let ours: Vec<usize> = p.lines.iter().map(|l| l.text.start as usize).collect();
        assert_eq!(ours, [0, 5, 11, 17], "at {w}: one line per phrase");
        assert!(ours[1..].iter().all(|s| allowed.contains(s)));
        let l = oracle.layout(text, &spans, Some(w));
        let theirs: Vec<usize> = l.lines().map(|l| l.text_range().start).collect();
        // Parley's extra starts are right after a no-break space.
        let extra: Vec<usize> = theirs
            .iter()
            .copied()
            .filter(|s| !ours.contains(s))
            .collect();
        assert!(!extra.is_empty(), "at {w}: Parley no longer differs");
        for s in extra {
            let before = text[..s].chars().next_back().unwrap();
            assert!(
                matches!(before, '\u{00A0}' | '\u{202F}' | '\u{2007}'),
                "at {w}: {s}"
            );
        }
    }
}
