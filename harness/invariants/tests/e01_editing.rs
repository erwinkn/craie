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
            "wrapped-words",
            "one two three four five six",
            16.0,
            Some(40.0),
            vec![
                Move(TextStart),
                Move(WordRight),
                Move(WordRight),
                Move(WordRight),
                Move(WordRight),
                Move(WordRight),
                Move(WordRight),
                Move(WordLeft),
                Move(WordLeft),
                Move(WordLeft),
                Move(WordLeft),
                Select(WordRight),
                Select(WordRight),
                Select(WordLeft),
                WordAt(10.0, 30.0),
                Extend(30.0, 60.0),
                WordAt(20.0, 50.0),
                Move(TextEnd),
                BackspaceWord,
                BackspaceWord,
                Move(TextStart),
                DeleteWord,
                DeleteWord,
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

/// What one side shows after a step: its edit state, and its caret (x
/// and line box top), if drawn.
#[derive(Clone, Debug)]
struct Observed {
    state: EditState,
    caret: Option<(f32, f32)>,
}

fn observe_ours(ed: &craie_text::editor::Editor) -> Observed {
    Observed {
        state: e01::state_ours(ed),
        caret: ed.caret_rect(0.0).map(|c| (c.0, c.1)),
    }
}

fn observe_parley(ed: &parley::PlainEditor<u16>) -> Observed {
    let caret = ed.cursor_geometry(0.0).map(|g| {
        // Parley's caret starts at the line's selection box
        // (`block_min_coord`, clamped with negative leading); the line box
        // top is the comparable value (E01).
        let line = ed
            .try_layout()
            .unwrap()
            .lines()
            .find(|l| l.metrics().block_min_coord == g.y0 as f32)
            .expect("caret line");
        let m = line.metrics();
        (g.x0 as f32, m.baseline - m.ascent - m.leading * 0.5)
    });
    Observed {
        state: e01::state_parley(ed),
        caret,
    }
}

/// The difference between two sides, if any: the edit state exactly,
/// whether a caret is drawn, and the caret x within `bound` (the f32
/// bound of a line's additions: every cluster edge sums the advances
/// before it) and its line box top.
fn diff(a: &Observed, b: &Observed, bound: f32) -> Option<String> {
    if a.state != b.state {
        return Some(format!(
            "state\n  ours   {:?}\n  parley {:?}",
            a.state, b.state
        ));
    }
    match (a.caret, b.caret) {
        (None, None) => None,
        (Some(c), Some(g)) => {
            let off = (c.0 - g.0).abs() > bound || (c.1 - g.1).abs() > 1e-3 * (1.0 + g.1.abs());
            off.then(|| format!("caret ours {c:?} parley {g:?}"))
        }
        (c, g) => Some(format!("caret presence ours {c:?} parley {g:?}")),
    }
}

/// The caret x bound for `ed`'s layout.
fn caret_bound(ed: &craie_text::editor::Editor, x: f32) -> f32 {
    let n = ed.layout().glyphs.len() + 4;
    let s = ed.layout().width + x.abs();
    2.0 * (n as f32 * f32::EPSILON / 2.0) * s + s * f32::EPSILON
}

#[test]
fn owned_editor_matches_parley() {
    let mut engine = e01::engine();
    let mut oracle = Oracle::new();
    let mut failures: Vec<String> = Vec::new();
    let mut steps_run = 0;
    for (name, text, size, width, steps) in scripts() {
        let mut ours = e01::editor(text, size, width, &mut engine);
        let mut theirs = oracle.editor(text, size, width);
        for (k, op) in steps.iter().enumerate() {
            e01::apply_ours(&mut ours, &mut engine, op);
            oracle.apply(&mut theirs, op);
            steps_run += 1;
            let (a, b) = (observe_ours(&ours), observe_parley(&theirs));
            let bound = caret_bound(&ours, b.caret.map_or(0.0, |c| c.0));
            if let Some(d) = diff(&a, &b, bound) {
                failures.push(format!("{name} step {k} {op:?}: {d}"));
            }
        }
    }
    for f in &failures {
        eprintln!("{f}");
    }
    assert!(failures.is_empty(), "{} differences", failures.len());
    assert_eq!(steps_run, 150, "every scripted step ran");
}

/// Negative controls: `diff` finds each kind of difference.
#[test]
fn diff_detects_each_difference() {
    let base = Observed {
        state: EditState {
            text: "ab".into(),
            anchor: 1,
            focus: 1,
            compose: None,
        },
        caret: Some((10.0, 0.0)),
    };
    assert!(diff(&base, &base, 1e-4).is_none());
    let mut m = base.clone();
    m.state.focus = 2;
    assert!(diff(&m, &base, 1e-4).is_some(), "selection");
    let mut m = base.clone();
    m.state.text = "abc".into();
    assert!(diff(&m, &base, 1e-4).is_some(), "text");
    let mut m = base.clone();
    m.state.compose = Some(0..1);
    assert!(diff(&m, &base, 1e-4).is_some(), "preedit");
    let mut m = base.clone();
    m.caret = None;
    assert!(diff(&m, &base, 1e-4).is_some(), "caret presence");
    assert!(
        diff(&base, &m, 1e-4).is_some(),
        "caret presence, other side"
    );
    let mut m = base.clone();
    m.caret = Some((10.01, 0.0));
    assert!(diff(&m, &base, 1e-4).is_some(), "caret x");
    let mut m = base.clone();
    m.caret = Some((10.0, 0.5));
    assert!(diff(&m, &base, 1e-4).is_some(), "caret line");
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
    // The exact traces, repeats at the end included. Ours: every grapheme
    // start, then the text end. Parley: also 1 (between e and its
    // accent) and 9, 12, 16 (inside the family's ligature).
    assert_eq!(a, [0, 3, 4, 5, 23, 24, 25, 25, 25]);
    assert_eq!(b, [0, 1, 3, 4, 5, 9, 12, 16, 19]);
    let mut stops = a.clone();
    stops.dedup();
    assert_eq!(stops, graphemes, "ours stops at each grapheme");

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
