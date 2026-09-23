//! E01 correctness: the owned paragraph against Parley on the pinned
//! fonts, per case and width. Line breaks, cluster mappings (text range
//! and glyph ids per group), and drawn glyph ids must match exactly,
//! except where UAX #9 rule L1 alone separates the two (`e01::Diff`);
//! advances bit-equal; positions and line metrics within the f32
//! summation bound of `e01::compare`.

use craie_harness::e01::{self, Oracle};

#[test]
fn owned_paragraph_matches_parley() {
    let mut engine = e01::engine();
    let mut oracle = Oracle::new();
    let mut failures = Vec::new();
    for case in e01::cases() {
        for &w in &case.widths {
            let p = e01::layout(&mut engine, &case.text, &case.spans, w);
            // The pinned fonts cover every case: no .notdef anywhere.
            assert!(p.glyphs.iter().all(|g| g.id != 0), "{}: .notdef", case.name);
            let l = oracle.layout(&case.text, &case.spans, w);
            let d = e01::compare(
                &e01::ours(&p, &case.text),
                &e01::parley(&l, &case.text),
                &case.text,
            );
            eprintln!(
                "{:14} {:13} w={:>5?}: breaks {} clusters {} ids {} l1 {} adv {:.2e} pos {:.2e} metrics {:.2e} worst {:.3}",
                case.name,
                case.class,
                w,
                d.line_breaks,
                d.clusters,
                d.glyph_ids,
                d.l1_lines,
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
                    e01::ours(&p, &case.text),
                    e01::ours(&fresh, &case.text),
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
    let a = e01::ours(
        &e01::layout(&mut engine, &case.text, &case.spans, w),
        &case.text,
    );
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

    let mut m = a.clone();
    m.lines[2].glyphs[1].x += 0.01;
    let d = e01::compare(&m, &b, &case.text);
    assert!(d.exact_structure() && !d.pass(), "{d:?}");
}
