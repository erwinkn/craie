//! E01 editable cases: the owned editor against Parley's `PlainEditor`
//! on the pinned fonts. The same scripted steps run on both; after each,
//! the buffer (preedit included), the selection's anchor and focus bytes,
//! and the preedit range must be equal. Where Parley puts a caret inside
//! a grapheme, a separate test asserts the exact difference.

use craie_harness::e01::{self, EditState, Op, Oracle};
use craie_text::editor::Motion::*;

/// (name, text, font size, wrap width, steps).
type Script = (&'static str, &'static str, f32, Option<f32>, Vec<Op>);

fn scripts() -> Vec<Script> {
    use Op::*;
    vec![
        (
            "ltr",
            "Hello world, the quick fox.",
            16.0,
            None,
            vec![
                Move(TextEnd),
                Backspace,
                Backspace,
                Insert("!"),
                Move(WordLeft),
                Move(WordLeft),
                Move(WordLeft),
                Move(Left),
                Select(WordRight),
                Insert("X"),
                Move(TextStart),
                Move(Right),
                Move(Right),
                Move(Right),
                Delete,
                DeleteWord,
                BackspaceWord,
                Move(WordRight),
                Move(WordRight),
                Select(WordLeft),
                Select(Left),
                SelectAll,
                Delete,
                Insert("new"),
                Move(LineStart),
                Select(LineEnd),
            ],
        ),
        (
            "wrapped",
            "The quick brown fox jumps over the lazy dog, then keeps running past the old mill.",
            16.0,
            Some(120.0),
            vec![
                Move(TextStart),
                Move(Down),
                Move(Down),
                Move(Down),
                Move(Up),
                Move(LineEnd),
                Move(LineStart),
                Select(Down),
                Select(LineEnd),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(LineEnd),
                Move(Right),
                Move(Right),
                Move(Left),
                Move(Left),
                Move(Left),
                Point(50.0, 30.0),
                Extend(100.0, 50.0),
                WordAt(30.0, 10.0),
                Extend(90.0, 45.0),
                Move(TextEnd),
                Move(Up),
                Move(Up),
                Select(TextStart),
            ],
        ),
        (
            "hard-breaks",
            "one\ntwo three\n\nfour",
            16.0,
            None,
            vec![
                Move(TextStart),
                Move(Down),
                Move(Down),
                Move(Down),
                Move(Up),
                Move(LineEnd),
                Move(Right),
                Move(Left),
                Move(LineEnd),
                Delete,
                Insert("\n"),
                Move(TextEnd),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Backspace,
                Select(Up),
                Select(Up),
            ],
        ),
        (
            "bidi-ltr",
            "abc אבג def",
            16.0,
            None,
            vec![
                Move(TextStart),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(WordRight),
                Move(WordRight),
                Move(WordRight),
                Move(WordLeft),
                Move(WordLeft),
                Move(TextEnd),
                Backspace,
                Select(TextStart),
            ],
        ),
        (
            "bidi-rtl",
            "שלום עולם abc",
            16.0,
            None,
            vec![
                Move(TextStart),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Left),
                Move(Right),
                Move(Right),
                Move(Right),
                Move(WordLeft),
                Move(WordLeft),
                Move(WordRight),
                Move(TextEnd),
                Backspace,
                Move(LineStart),
                Select(LineEnd),
            ],
        ),
        (
            "compose",
            "ab",
            16.0,
            None,
            vec![
                Move(TextEnd),
                Compose("かな", Some((6, 6))),
                Compose("かなn", Some((7, 7))),
                FinishCompose,
                Compose("x", None),
                ClearCompose,
                Insert("y"),
                Range(0, 2),
                Compose("zz", Some((0, 2))),
                FinishCompose,
                Move(TextEnd),
            ],
        ),
    ]
}

#[test]
fn owned_editor_matches_parley() {
    let mut engine = e01::engine();
    let mut oracle = Oracle::new();
    let mut failures: Vec<String> = Vec::new();
    for (name, text, size, width, steps) in scripts() {
        let mut ours = e01::editor(text, size, width, &mut engine);
        let mut theirs = oracle.editor(text, size, width);
        for (k, op) in steps.iter().enumerate() {
            e01::apply_ours(&mut ours, &mut engine, op);
            oracle.apply(&mut theirs, op);
            let (a, b): (EditState, EditState) =
                (e01::state_ours(&ours), e01::state_parley(&theirs));
            if a != b {
                failures.push(format!(
                    "{name} step {k} {op:?}:\n  ours   {a:?}\n  parley {b:?}"
                ));
                continue;
            }
            // The caret draws at the same place: x within the f32 bound of
            // a line's additions (every cluster edge sums the advances
            // before it), and the same line box top.
            if let (Some(c), Some(g)) = (ours.caret_rect(0.0), theirs.cursor_geometry(0.0)) {
                // Parley's caret starts at the line's selection box
                // (`block_min_coord`, clamped with negative leading); the
                // line box top is the comparable value (E01).
                let layout = theirs.try_layout().unwrap();
                let line = layout
                    .lines()
                    .find(|l| l.metrics().block_min_coord == g.y0 as f32)
                    .expect("caret line");
                let m = line.metrics();
                let (x, y) = (g.x0 as f32, m.baseline - m.ascent - m.leading * 0.5);
                let n = ours.layout().glyphs.len() + 4;
                let s_x = ours.layout().width + x.abs();
                let bound = 2.0 * (n as f32 * f32::EPSILON / 2.0) * s_x + s_x * f32::EPSILON;
                if (c.0 - x).abs() > bound || (c.1 - y).abs() > 1e-3 * (1.0 + y.abs()) {
                    failures.push(format!(
                        "{name} step {k} {op:?}: caret ours {:?} parley {:?}",
                        (c.0, c.1),
                        (x, y)
                    ));
                }
            }
        }
    }
    for f in &failures {
        eprintln!("{f}");
    }
    assert!(failures.is_empty(), "{} differences", failures.len());
}

/// Carets stop at graphemes (UAX #29), where Parley differs: moving right
/// through a decomposed é and a ZWJ family, ours stops at each grapheme
/// start; Parley also stops between e and its accent, and inside the
/// family's ligature. Backspace after a combining mark removes the mark
/// (one code point, as Parley); after an emoji, the whole grapheme.
#[test]
fn caret_stops_at_graphemes_where_parley_splits() {
    use unicode_segmentation::UnicodeSegmentation;
    let text = "e\u{301}a 👨\u{200D}👩\u{200D}👧 x";
    let graphemes: Vec<usize> = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let mut engine = e01::engine();
    let mut oracle = Oracle::new();
    let mut ours = e01::editor(text, 16.0, None, &mut engine);
    let mut theirs = oracle.editor(text, 16.0, None);
    let (mut a, mut b) = (vec![0usize], vec![0usize]);
    for _ in 0..8 {
        e01::apply_ours(&mut ours, &mut engine, &Op::Move(Right));
        oracle.apply(&mut theirs, &Op::Move(Right));
        a.push(e01::state_ours(&ours).focus);
        b.push(e01::state_parley(&theirs).focus);
    }
    a.dedup();
    b.dedup();
    assert_eq!(a, graphemes, "ours stops at each grapheme");
    assert!(
        b.iter().any(|i| !graphemes.contains(i)),
        "Parley no longer splits: {b:?}"
    );

    let at = |i: u32| {
        let mut ed = e01::editor(text, 16.0, None, &mut e01::engine());
        ed.select_byte_range(i, i);
        ed
    };
    let mut engine = e01::engine();
    let mut ed = at(3);
    ed.backdelete(&mut engine);
    assert_eq!(ed.raw_text(), "ea 👨\u{200D}👩\u{200D}👧 x");
    let mut ed = at(23);
    ed.backdelete(&mut engine);
    assert_eq!(ed.raw_text(), "e\u{301}a  x");
}
