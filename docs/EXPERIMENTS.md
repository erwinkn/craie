# Experiments

Findings per milestone. Each entry is a decision or measurement with the
reason it landed. Proposals live in `ARCHITECTURE.md`, in the
`Experiments` section of each subsystem; a proposal becomes an entry here
only when it has a measured result. Decisions in `ARCHITECTURE.md`
override older entries below.

## Step 1 — crate split, CRW2, retained scene (2026-09-23)

Conditions: the shared M5 Max ran under heavy load (load average 12 to
93). Wall-clock varies about 40 percent run to run, so baseline
(`f26c22b`) and step 1 (`e61b9da`) ran back to back, three repetitions
each, and the table gives medians. Allocation counts, bytes, and
primitive counts are exact.

### Wire

| transaction              | CRW1      | CRW2, no span interning | CRW2      |
|--------------------------|-----------|-------------------------|-----------|
| 5k-row mount             | 683,073 B | 743,072 B               | 663,088 B |
| 2k-message transcript    | 371,692 B | 423,181 B               | 351,229 B |

A paragraph op plus a 16-byte span row costs more than CRW1's
set_text + text_props. Interning span lists per transaction, like
styles, recovers it: most paragraphs share one style.

### `examples/bench` (5k uniform rows, 10,001 nodes; transcript 10,501)

| phase                          | baseline            | step 1              |
|--------------------------------|---------------------|---------------------|
| decode + apply                 | 2.0 ms, 10,121 allocs | 7.1 ms*, 10,212 allocs |
| layout, cold                   | 650 ms, 746,811 allocs | 688 ms, 746,834 allocs |
| paint, cold                    | 3.9 ms, 263 allocs  | 3.7 ms, 360 allocs  |
| paint, warm unchanged          | 0.10 ms, 12 allocs  | 0.00 ms, 0 allocs   |
| layout, warm unchanged         | 0.12 ms             | 0.00 ms (skipped)   |
| layout, 500 dirty texts        | 43 ms, 67,013 allocs | 27 ms, 67,027 allocs |
| paint, 500 dirty texts         | 0.40 ms             | 0.41 ms             |
| chunks built, cold             | n/a                 | 70 of 10,001        |
| first upload                   | 84 KB instances + 121 KB atlas | 207 KB tables + 121 KB atlas |
| warm upload                    | 84 KB (every draw)  | 0 bytes             |
| draw submit, warm              | 0.83 ms             | 0.31 ms             |
| transcript: paint per token    | 0.092 ms            | 0.013 ms            |
| transcript: layout per token   | 2.53 ms             | 2.62 ms             |
| live heap, 5k rows             | 36.4 MiB            | 43.8 MiB            |

\* decode + apply wall-clock is noise-dominated (1.7 to 8.6 ms across
all six runs); its allocation count moved by 0.9 percent.

The live heap grew 7.4 MiB. Struct sizes account for about 5.3 MiB:
dense per-node host rows cost 348 bytes (layout 240, paragraph 48,
spatial 36, paint 16, interaction 8) and scene bookkeeping 180 bytes
(chunk 92, node space 72, placement 16), against about 70 bytes per
node before. The layout row is the decision "layout inputs are
per-node rows"; a compact owned row arrives with the owned flex engine.

Why the warm and streaming paths improved: an unchanged frame skips
layout and does no scene work at all; a text change rebuilds one chunk;
a layout move patches placements of moved subtrees only.

### `examples/framebench` (real React commits, medians of medians)

| rows  | phase        | baseline | step 1 |
|-------|--------------|----------|--------|
| 100   | firstDraw    | 7.45 ms  | 3.08 ms |
| 100   | update.draw  | 0.194 ms | 0.157 ms |
| 100   | scroll.draw  | 0.349 ms | 0.066 ms |
| 1,000 | firstDraw    | 33.2 ms  | 16.9 ms |
| 1,000 | scroll.draw  | 0.899 ms | 0.543 ms |
| 5,000 | firstDraw    | 98.6 ms  | 69.7 ms |
| 5,000 | update.draw  | 0.220 ms | 0.231 ms |
| 5,000 | scroll.draw  | 2.67 ms  | 2.79 ms |
| 5,000 | remove       | 3.35 ms  | 3.50 ms |
| 5,000 | emptyDraw    | 0.211 ms | 0.253 ms |

Framebench scrolls by changing `marginTop` (a layout change), so its
scroll moves every row; the native scroll path (one record, 32 bytes)
is not what it measures. The 5k-row differences are inside the noise
band. First-draw live bytes: +27.2 MiB before, +28.3 MiB after.

### E01: owned paragraph versus Parley (step 3a)

Correctness: `cargo test -p craie-harness --test e01_text`. Cost:
`cargo run --release -p craie-harness --example e01_text`. Both sides
get the same pinned font bytes (`assets/fonts`) and the same fallback
order (Parley through a font stack of the pinned families; Craie
through `RawFonts` registration order). Thirteen cases at 34 widths.
Wrapped: Latin at 120/240/480 and unbounded, and at 40/90 with an
overflowing word. Styled: bold, italic, and 24-pt spans, and bold and
italic Arabic and Hebrew with synthesis. Multilingual: Latin, Devanagari
conjuncts, Japanese, ✕, and an em dash. Bidi: an LTR paragraph with
Hebrew, Arabic, and numbers; an RTL paragraph with English and
Arabic-Indic digits; a Hebrew paragraph with newlines and an empty
line. Breaks: LF, CR, LS, and PS, at widths down to 0. Emoji: ZWJ
sequences, a skin tone, keycaps, a flag, and VS16. Fallback: stacked
Latin marks, Arabic shadda and keycap on a digit, a dotted circle.
Missing: Thai and Ethiopic, which no pinned face covers (both sides
draw `.notdef`; every other case asserts none). Ligatures: Latin ffi
across three graphemes, and marked Arabic. Editable cases are step 3b.
`grapheme_and_ligature_clusters_are_exact` asserts the literal clusters
of स्ते (bytes 0..12, one line at any width, hits only on its edges,
exactly one oracle merge) and of the ffi ligature.

Correctness per case (every width):

| case (class)             | line breaks | cluster maps | drawn glyphs | advances  | worst position or metric / bound |
|--------------------------|-------------|--------------|--------------|-----------|----------------------------------|
| latin (wrapped)          | equal       | equal        | equal        | bit-equal | 0.072                            |
| latin-narrow (wrapped)   | equal       | equal        | equal        | bit-equal | 0.069 (largest error 9.8e-4 pt)  |
| styled (styled)          | equal       | equal        | equal        | bit-equal | 0.028                            |
| styled-synth (styled)    | equal       | equal        | equal        | bit-equal | 0.128                            |
| multilingual             | equal       | equal after 1 GB9c merge | equal | bit-equal | 0.076                  |
| bidi-ltr (bidi)          | equal       | equal        | equal but 2 L1 lines | bit-equal | 0.054                    |
| bidi-rtl (bidi)          | equal       | equal        | equal but 3 L1 lines | bit-equal | 0.070                    |
| hebrew-lines (bidi)      | equal       | equal        | equal        | bit-equal | 0.064                            |
| breaks                   | equal       | equal        | equal        | bit-equal | 0.027                            |
| emoji                    | equal       | equal        | equal        | bit-equal | 0.067                            |
| fallback                 | equal       | equal        | equal        | bit-equal | 0.000                            |
| missing                  | equal       | equal        | equal        | bit-equal | 0.025                            |
| ligatures (unbounded)    | equal       | equal        | equal        | bit-equal | 0.028                            |

A drawn glyph is compared by glyph id, source cluster, and the pinned
file it comes from, in visual order. A cluster map is each glyph
group's text range and glyph ids in logical order (Parley's ligature
continuation clusters merge into their group). `byte_to_cluster_matches_parley`
checks the byte-to-cluster query at every byte of every case against
Parley's `Cluster::from_byte_index`, and cluster-to-byte at each
cluster's first and last byte. The bound for a position or metric is
the f32 error of the additions that produce it on each side (2γₖ·S
plus one ulp, with k additions and S the sum of term magnitudes). The
table gives the worst ratio of error to bound.

Known differences. Craie follows the Unicode rule in each; each has a
test that asserts the exact difference:

- UAX #9 L1 at soft line ends. Craie moves a line's trailing whitespace
  to the paragraph level; Parley keeps it in the embedded run, so the
  visible glyphs of 5 wrapped lines differ by one space advance. The
  comparison applies L1 to Parley's line: ids, clusters, and fonts must
  then be equal and positions within bound. `line_order_matches_unicode_bidi`
  checks Craie's order against unicode-bidi's own L1 and L2, and
  `mapping_reads_the_placements` checks the drawn cluster order of
  L1-reversed segments.
- Graphemes (UAX #29 GB9c). A Craie cluster never splits a grapheme; a
  Devanagari conjunct joined by a virama (स्ते) is one grapheme. Parley
  splits it in two. The comparison merges Parley's clusters inside one
  grapheme and counts them (1 in the multilingual case).
- Hard breaks (UAX #14 LB5). Parley breaks after CR and again after LF
  in a CRLF (an extra line that holds the LF), and does not break after
  NEL. Craie's line starts are exactly UAX #14's mandatory breaks
  (`hard_breaks_follow_uax14_where_parley_differs`).
- No-break spaces (LB12). At narrow widths Parley breaks after NBSP,
  narrow NBSP, and figure space. Craie keeps each protected phrase on
  one line (`no_break_spaces_follow_uax14_where_parley_differs`).
- Breaks inside a marked grapheme (LB9). At 40 pt, Parley starts a
  line between an Arabic base and its kasra. Craie's line starts are
  UAX #14 opportunities (`marked_arabic_breaks_follow_uax14_where_parley_differs`).
- One font per grapheme. For a dotted circle with a Hebrew point, which
  Noto Sans covers in part, Craie draws both from Noto Sans Hebrew;
  Parley draws the base from Noto Sans and the mark from Noto Sans
  Hebrew (`grapheme_keeps_one_font_where_parley_splits`).
- Selection box with negative leading. When a line mixes fonts,
  Parley's `block_min_coord` clamps the box to ascent plus descent, so
  it extends above the line box. Craie's line top and its selection
  rectangles are the line box. Line tops, heights, and baselines agree.
- Oracle metrics. Parley's `trailing_whitespace` counts only a visual
  last run, so on L1 lines it reads 0. With emoji ZWJ ligatures,
  Parley's `LineMetrics::advance` is not the sum of its own glyph
  advances (1.47 pt short on a 306 pt line; its run advances sum
  correctly). The comparison keeps the raw metric on every line except
  a verified defect line (a multi-codepoint emoji sequence, with
  Parley's run advances equal to its glyph sum within the f32 bound),
  where it uses the glyph sum. Each case asserts the expected count per
  width (emoji: 1, 2, 2; all others 0), and two negative controls fail:
  a changed advance on a non-emoji line, and the emoji case without the
  flag (`emoji_advance_correction_is_scoped`).
- Oracle normalization. Parley lists a right-to-left ligature
  continuation (glyph-less) before its ligature start in logical order;
  the comparison joins it to the start after it, not to the cluster
  before, and does not count it as a grapheme merge.

Cost at each case's first bounded width (median of 1,001 interleaved
calls; host load average 30 to 85 on 18 cores during this round, so
absolute times move up to 3x between runs; ratios, bytes, and
allocation counts are the reliable part):

| case         | text B | retained B ours / Parley | cold µs ours / Parley | cold allocs | rewrap µs   | rewrap allocs | edit µs     | edit allocs |
|--------------|--------|--------------------------|-----------------------|-------------|-------------|---------------|-------------|-------------|
| latin        | 386    | 13,662 / 18,336          | 77.6 / 85.8           | 11 / 27     | 3.88 / 2.62 | 2 / 2         | 57.6 / 61.5 | 13 / 30     |
| latin-narrow | 386    | 20,958 / 32,928          | 51.5 / 55.5           | 15 / 31     | 6.04 / 5.17 | 0 / 0         | 79.8 / 89.1 | 15 / 31     |
| styled       | 70     | 2,610 / 5,796            | 16.0 / 21.8           | 6 / 18      | 0.46 / 0.38 | 2 / 2         | 7.3 / 10.3  | 6 / 18      |
| styled-synth | 35     | 2,071 / 3,520            | 5.4 / 7.1             | 20 / 16     | 0.21 / 0.21 | 1 / 1         | 5.5 / 7.2   | 20 / 16     |
| multilingual | 128    | 3,400 / 7,632            | 32.7 / 42.8           | 9 / 33      | 0.75 / 0.92 | 0 / 0         | 32.5 / 42.6 | 9 / 45      |
| bidi-ltr     | 55     | 3,227 / 6,288            | 9.1 / 11.5            | 24 / 21     | 0.33 / 0.33 | 1 / 1         | 9.6 / 12.3  | 24 / 21     |
| bidi-rtl     | 96     | 5,644 / 12,560           | 36.8 / 41.4           | 30 / 29     | 1.17 / 1.04 | 0 / 0         | 37.1 / 41.7 | 30 / 29     |
| breaks       | 39     | 1,663 / 4,016            | 3.9 / 5.5             | 7 / 17      | 0.25 / 0.25 | 0 / 0         | 4.0 / 5.6   | 7 / 17      |
| emoji        | 98     | 3,134 / 10,704           | 8.8 / 15.1            | 9 / 27      | 0.29 / 0.42 | 0 / 0         | 19.8 / 32.5 | 9 / 27      |
| fallback     | 22     | 1,002 / 3,216            | 7.1 / 11.5            | 6 / 16      | 0.21 / 0.25 | 0 / 0         | 7.3 / 11.8  | 6 / 16      |
| missing      | 46     | 1,618 / 3,936            | 9.5 / 15.6            | 7 / 25      | 0.46 / 0.50 | 0 / 0         | 9.5 / 15.7  | 7 / 25      |
| hebrew-lines | 72     | 2,568 / 6,864            | 20.0 / 23.1           | 32 / 24     | 0.38 / 0.38 | 0 / 0         | 9.9 / 11.8  | 33 / 27     |

- Retained bytes: what dropping the laid-out paragraph frees.
  `Paragraph::heap_bytes` gives the same number. Craie holds 0.29 to
  0.75 times Parley's bytes. The 28-byte glyph row is most of it
  (Latin: 10.8 KB of 13.7 KB).
- Cold: a new paragraph on a warm engine. Craie takes 0.58 to 0.93
  times Parley's time. It makes fewer allocations except on bidi text
  (`BidiInfo` allocates its level and class tables; an LTR-only
  paragraph skips it).
- Rewrap (a width change): no shaping on either side, and no
  allocation once the line and segment stores have grown (the 1 or 2
  counted allocations are that growth). Craie takes 0.69 to 1.48 times
  Parley's time. It is slower on Latin at 120 (1.48), latin-narrow
  (1.17), and styled (1.21): Craie rebuilds its cluster scratch from
  the glyph store on each rewrap, and Parley keeps its clusters. An
  accepted trade (LEDGER.md AR-2), with a follow-up E01 measurement of
  cluster reconstruction against placement traversal.
- Edit (one character inserted in the middle): both lay the paragraph
  out again. Incremental reflow is E04.
- Span color: Craie does no layout work (the span index is the paint
  slot; `costs.rs` asserts zero shapes and zero layouts). Parley stores
  the brush in the layout's styles, so a new color needs a new layout
  (the cold cost).
- A fresh engine's first multilingual layout (font parsing, shaping
  data, plans): 0.9 to 2.6 ms for Craie, 0.6 to 2.5 ms for Parley
  (eight runs).

Changes that came from this measurement:
- HarfRust compiled a shape plan on each `shape` call. Plans are now
  cached per (instance, direction, script). Cold allocations went from
  137 to 473 down to 6 to 32.
- `unicode_bidi::visual_runs` clones the levels of the whole paragraph
  for each line (O(text × lines)). Rewrap now keeps the analysis flags
  from shaping and does L1 and L2 over the line's clusters, in place.
  Rewrap allocations went from 33 to 232 down to 0 to 2.
- Coverage checks parsed the font's cmap for each character. Each face
  now has an ASCII coverage mask and a coverage cache, and the primary
  instance is resolved once per span.
- The engine made Parley's `FontContext` (a system font scan, 15 to 47
  ms) when it was created. It now makes it when an input first needs
  it.

First draw and the default font (review round 1, S3A-10). Step 3a
changed the Text default from Parley's `sans-serif` (Helvetica) to
`system-ui` (SF; ARCHITECTURE.md §5, Decisions). Framebench on the same
React wire dumps, `f622135` and step 3a back to back, two rounds of 3
repetitions (medians; load average 30 to 85, so ±20%):

| rows  | step 2, Helvetica | 3a, Helvetica (`--family sans-serif`) | 3a, SF (default) |
|-------|-------------------|---------------------------------------|------------------|
| 100   | 5.69 ms           | 4.34 ms                               | 7.12 ms          |
| 1,000 | 21.2 ms           | 18.0 ms                               | 31.2 ms          |
| 5,000 | 93.4 ms           | 83.3 ms                               | 101.8 ms         |
| 5,000 first-draw live bytes | +27.7 MiB | +21.0 MiB               | +21.1 MiB        |

On the same font the owned engine draws the first frame faster than
step 2 at every size; the increase over step 2 comes from SF.
`examples/fontcost` splits a fresh engine's font work for the row text
(one process per engine and family, so caches start cold). Both engines
resolve the same faces (Helvetica, .SF NS). The owned engine's one-time
work is small for both fonts: family resolution about 0.05 ms, HarfRust
shaping data about 0.02 ms, plan and first shape 0.1 to 0.3 ms, and 0
fallback queries (the ASCII row needs none). Source loading (fontique's
system scan, 28 to 50 ms, both engines) happens in `Ui::new`, outside
the first draw. The exact cost that SF adds is raster work: 1,000 rows
need 110 rasterizations in SF and 71 in Helvetica, on both engines.
Shaping and emission times for 1,000 rows moved up to 3x between runs
under this load, so no time split is claimed for them.

### E01 editable cases: owned editing versus Parley's editor (step 3b)

Correctness: `cargo test -p craie-harness --test e01_editing`. The
owned `Editor` and Parley's `PlainEditor` (the pinned fonts, Parley
unquantized) run the same scripted steps: typing, Backspace and Delete
by cluster and by word, visual left and right, word moves, line
start and end, up and down across soft and hard breaks, text start and
end, selection versions of each, select all, click, drag, double-click
word selection and word-wise drag, byte-range selection, and IME
preedit, commit, and cancel. Seven scripts: Latin, a wrapped paragraph
at 120 pt, hard breaks with an empty line, bidi in an LTR paragraph and
in an RTL one, word moves, word selection, and word deletion across
soft breaks at 40 pt, and IME composition (150 steps, a count the test
asserts). After every step the buffer (preedit included), the
selection's anchor and focus bytes, and the preedit range are equal;
both sides draw a caret or neither does; and the caret draws at the
same x (within the f32 bound of the line's additions) and at the same
line box top. `diff_detects_each_difference` proves the comparison
finds a changed selection, text, preedit, caret presence (either side),
caret x, and caret line.

Known difference, asserted exactly
(`caret_stops_at_graphemes_where_parley_splits`, both traces literal,
repeats included): Parley puts a caret between e and a combining
acute, and inside the ligature of a ZWJ family; Backspace there leaves
an orphan mark. Craie's caret stops only
at grapheme boundaries (UAX #29); Backspace after a combining mark
removes the mark (one code point, as Parley), and after an emoji the
whole grapheme.

Cost (`e01_text` example, `editing` rows; interleaved medians of 1,001
calls; load average 14 to 19):

| text (bytes)       | retained B ours / Parley | keystroke pair µs | pair allocs | caret move µs | move allocs | width change µs | width allocs |
|--------------------|--------------------------|-------------------|-------------|---------------|-------------|-----------------|--------------|
| latin (180)        | 7,996 / 8,848            | 80.5 / 79.2       | 30 / 41     | 0.08 / 0.04   | 0 / 0       | 2.04 / 30.29    | 2 / 22       |
| bidi (55)          | 3,658 / 6,547            | 33.2 / 37.4       | 60 / 43     | 0.04 / 0.04   | 0 / 0       | 0.42 / 12.92    | 0 / 21       |
| multilingual (74)  | 2,440 / 6,566            | 30.1 / 52.8       | 24 / 53     | 0.08 / 0.04   | 0 / 0       | 0.54 / 27.50    | 0 / 26       |

A keystroke pair is one character typed at the end and deleted: two
edits, each shaping the paragraph once on both sides. A width change
rewraps the owned paragraph; Parley's editor lays out again. The bidi
pair allocates more (`BidiInfo`, LEDGER.md AR-3).

Found while porting:
- The first caret-move version built the line's visual clusters as a
  `Vec` on each move (9 to 29 allocations, 20 to 40 times Parley's
  time). An iterator over the placements (`Paragraph::clusters_of`)
  made moves allocation-free; carets, hits, and selection rectangles
  use it too.
- The editor's cluster table was rebuilt by an in-place `collect` that
  kept the 16-byte capacity of its source for 8-byte entries (14.7 KB
  retained for the Latin editor); it is now refilled in place.
- The text demo, ported off Parley, drew regular text bold after a
  650-weight title in SF. swash keeps its variation coordinates in the
  scale context across scaler builders, and the rasterizer skipped
  setting them for a default instance, so the previous scaler's
  coordinates applied. This dates from step 3a (captures there used
  one weight per variable face). The rasterizer now always sets them;
  `default_instance_raster_does_not_inherit_coords` (a 1.9 KB variable
  Noto Emoji subset, `assets/fonts/NotoEmoji-Var-Test.ttf`) fails
  without the fix.

Review round 1 (Astra, own thread) found, and the round fixed with
tests: `set_text` could leave the selection inside a character (now
clamped onto boundaries at once); an edit overlapping the preedit could
leave its range inside a character (now it ends composing); the IME
undo step lost the text a composition replaced (now recorded when
composition starts, with the commit in the same step); word moves
stopped at soft line breaks (neighbours now cross lines, as Parley's);
`onChangeText` fired on caret moves (now only when the committed text
changes; a JS `setText` does not echo); undo dropped cursor affinity
(snapshots keep full cursors). Rounds 2 and 3 found
the composition undo group's lifecycle (it now ends only when an edit
changes the text, or on undo, redo, `setText`, finish, or commit; undo
entries are recorded only when an edit changes the text) and a missing
`onChangeText` when a composition finishes (ImeDone or focus loss).
`editing_never_leaves_char_boundaries`
runs 24 seeds of 300 random operations over multibyte, bidi, emoji,
and mark text, and asserts both cursors and the preedit range stay on
character boundaries.

The text demo lost an inline monospace span: a paragraph has one family
(its code line is all monospace). Parley's line-height override is
gone with it.

### Step 3c: span record, nested Text, per-span events, selection

- The span record (wire v3, 28 bytes) carries the style React Native
  allows on nested Text. E01 case `spacing` (letter spacing on a span,
  an absolute line height) equals Parley: advances bit-equal, worst
  ratio to the f32 bound 0.061. Families resolve once at apply
  (`span_family_resolves_when_applied`: a color change neither
  re-resolves nor reshapes; a family change does both).
- Decorations draw one rect per decorated span stretch
  (`decorations_draw_over_their_spans`). The text demo shows the inline
  monospace span again, and an underline.
- Nested Text: one native text node per outermost Text; nested edits
  and removals send one paragraph op (bun tests); a pointer event names
  its span (`text_pointer_events_carry_the_span`) and reaches the
  nested Text that owns it.
- Selection: drags across paragraphs in tree order, copy, select all,
  clearing, clamping after a text change, and highlight rects in exactly
  the selected paragraphs' chunks (`selection_spans_paragraphs_in_tree_order`,
  `selection_survives_a_shrinking_paragraph`).
- Review round 1 (Astra): the rebuild oracle now carries the selection
  and compares ranges and copied text; `Gen::select` toggles
  `selectable` and sets selections from its own random stream (the
  mutation stream of every seed is unchanged). With the refresh removed
  (after transactions and before paint), seed 5 fails at step 54. Unit tests cover an
  inserted and a growing paragraph, removed and reused endpoints, a
  domain made not selectable, side-by-side texts, and overlapping texts.
- Letter spacing splits shaping items, as Parley does: a ligature does
  not straddle a spacing change. Since the step 4 opening, a spaced run
  also shapes without optional ligatures (CSS Text; ARCHITECTURE.md
  section 5). `spaced_runs_drop_optional_ligatures` (office with
  spacing from its second f, affix and waffle): exactly office's fi
  and affix's ffi split into letters where Parley keeps them, every
  other cluster equals Parley's (glyphs and advances), each spaced
  letter takes its spacing, spaced Arabic keeps its glyphs, and the
  text unspaced equals Parley. (Before the decision, with Parley's
  rule, Parley's line advance disagreed with its own glyphs for spaced
  ligatures: its clusters spaced ligature continuations, its glyphs did
  not.) The case is not in the general list, so E01 timing tables are
  unchanged.
- Review round 2 (Astra): the paragraph revision is a u32 in a
  36-byte event record (an 8-bit one repeated after 256 owner
  replacements, and a queued event reached the last owner); a reused
  text slot resolves fonts at its first paragraph op; a hidden text
  root keeps its text (React `Activity`). Each fix has a regression test
  that fails with the fix removed.
- The E01 font check compared `include_bytes` addresses; release
  builds hold a second copy of the bytes in the harness, so four E01
  tests failed in release only. It now compares by content.

### Step 4: native animation driver

- Cost assertion (ARCHITECTURE.md section 12): transform and opacity
  tweens do zero layouts and zero shapes over every frame of the tween
  (`spatial_tweens_neither_lay_out_nor_shape`, counters). A width tween
  relayouts each frame and moves the next sibling
  (`width_tween_reflows_siblings`); a tween to `auto` resolves its end
  by one probe layout and restores `auto` on its final frame, and one
  from `auto` starts at the laid-out size (`size_tweens_to_and_from_auto`).
- Two oracles over the generated sequences (`tests/incremental.rs`,
  8 seeds x 60 steps, 1x and 2x). `Gen::animate` declares transitions
  and starts `Animate` tweens (curves and springs, delays) from its own
  random stream, so the mutation stream of each seed does not change.
  (1) Incremental equals rebuild, as before: the snapshot carries
  transitions. (2) At rest, the animated Ui equals a twin that got the
  same transactions without animation (transitions dropped, each
  `Animate` a plain set): the end state of every tween is its declared
  value. The first oracle cannot see that (a rebuild reads the rows the
  driver wrote): with the final frame's write removed, (1) passes and
  (2) fails (seed 1, step 14). A removed node's animations are dropped
  (`removed_nodes_drop_their_animations`, which fails with the drop
  removed).
- Timing functions: CSS cubic-bezier by Newton steps with a bisection
  fallback (ease-in-out 0.5 at the middle, ease 0.4085 at 0.25); damped
  springs from rest in closed form, ending once the offset stays within
  0.001 (from the envelope when underdamped, by bisection otherwise).
  Transforms interpolate as rotation x upper-triangular x translation
  (a rotation by angle, the shorter way; scales by factor); colors
  premultiplied.
- The wire fixture (JS encoder, Rust decoder) carries both ops.
- Framebench, step 3c end (`db18a47`) against step 4 (`b24fa38`),
  5,000 rows, same wire input, the two binaries alternated (6 runs x 3
  reps each, half with each build first). Load average 27 to 51 from
  other applications during the whole measurement, so the spread is
  wide: first draw 38 to 98 ms on both builds, medians 65.0 ms (3c) and
  70.2 ms (step 4); with step 4 run first its median was the lower one
  (60.4 vs 71.5 ms). No change is measurable at this load; update,
  scroll, and empty draws are inside the same spread (0.12 to 0.25,
  1.3 to 2.7, 0.06 to 0.34 ms). The driver's idle cost is one emptiness
  check per render and one per paint decision.

### Step 5a: path meshes in the scene and renderer

- `craie-vector` tessellation (lyon 1.0, `lyon_tessellation` 1.0.22):
  a 10 x 10 square fills 100 (exactly), a square with a same-wound hole
  fills 100 under nonzero and 84 under even-odd, a circle of radius 50
  at tolerance 0.05 is within 0.5% of pi r^2; a 10-long stroke 2 wide
  covers 20 (butt), 24 (square caps), and a round-capped one loses less
  than perimeter x tolerance against 20 + pi.
- Review rounds 1 to 3 found that multisampling a whole frame breaks
  analytic anti-aliasing (rects, glyphs, clips); paths now render in
  multisampled layers of their own (ARCHITECTURE.md section 11). The
  rendered-empty first multisampled frame (1 in 20 runs, 5 in 60
  single-threaded) did not recur in 160 runs after the change.
- GPU (headless, `crates/render/tests/paths.rs`, 15 tests): a filled
  circle is solid inside, clear outside, and blends along its edge
  (partial coverage from 4x MSAA; with the path layer single-sampled
  the test fails); rects and glyphs (unsnapped, anisotropic, reduced,
  enlarged) draw the same with and without a path elsewhere; nested
  clips matching a path change none of its pixels; a mesh run
  composites between the content around it in painter order;
  its edges are symmetric; a frame of rects only stays single-sampled;
  linear and radial gradients pad past their ends and interpolate
  premultiplied in sRGB (the midpoint of red to blue at t = 0.49 is
  (130, 0, 125)); a mesh inside a 0.5 opacity layer composites to 188
  (white over black in sRGB).

### E14: layout-aware virtualization, list versus a plain column (step 2)

`cargo run --release -p craie-harness --example e14_lists`: a scroller
at 480x720 @2x holding chat-like texts (2 to 60 words, 14 pt), either
as a `List` with the harness `ListDriver` playing React, or as a plain
column of every row. Mount is Ui creation, the item splice, and the
first range's rows until the range is stable (warm fonts). Rerun at
step 3a on the pinned fonts (Noto Sans; step 2 measured Parley on
system fonts, so rows, visits, and heaps changed). Loaded machine (load
average 20 or more): times are indicative.

| items | list mount | plain mount | scroll (in range) | jump | list heap | plain heap | layout visits |
|-------|-----------:|------------:|------------------:|-----:|----------:|-----------:|--------------:|
| 1k    | 5.7 ms  | 39.6 ms  | 0.003 ms (plain 0.026) | 1.12 ms | 4.5 MiB  | 11.9 MiB | 48 (plain 4,001) |
| 10k   | 6.4 ms  | 214.6 ms | 0.003 ms (plain 0.114) | 1.43 ms | 4.8 MiB  | 77.2 MiB | 48 (plain 40,001) |
| 100k  | 5.9 ms  | -        | 0.001 ms               | 0.53 ms | 7.6 MiB  | -        | 48 |
| 1M    | 45.2 ms | -        | 0.001 ms               | 0.53 ms | 32.7 MiB | -        | 48 |

About 4.5 MiB of each heap is fixed: the text engine's font data and
the Ui. Element bytes per item: `ItemDesc` 12 (`size_of`, measured),
extents 13 (f32 size, bool measured, f64 tree node), and the sorted
identity index 4: 29 in all. The measured live bytes per added item,
from the exact counts at 1k and 1M items, are (34,309,449 − 4,707,273)
B / 999,000 = 29.63 B. The 0.63 B above the element sum is capacity
beyond length in those vectors (not broken down further). A list grown
by many splices can hold up to 2x per vector in capacity. 19 rows
render.

Identity index, build plus one query per id (ms), and the native path
(one splice mounting the list, then single-item appends):

| ids | sorted vec | std HashSet (SipHash, keyed) | multiplicative hash | native mount | append |
|-----|-----------:|-----------------------------:|--------------------:|-------------:|-------:|
| `k << 20`, 4,096 | 0.03 | 0.05 | 1.68 | 0.2 ms | 1 µs |
| sequential, 1M   | 19.9 | 30.8 | 6.4   | 44.5 ms | 2 µs |
| stride 2^11, 1M  | 22.8 | 39.8 | 228.8 | 22.9 ms | 5 µs |
| random, 1M       | 78.5 | 53.6 | 12.7  | 53.2 ms | 9 µs |

The multiplicative hash (round 2) degrades on chosen patterns (30x at
4,096 ids, 36x at 1M strided): wire ids are adversarial, so it went.
The sorted vector has no pattern to aim at and costs 4 bytes per id.
An update with n ids, r removed, and a added is one linear merge,
O(n + r log r + a log a), or in place for a few ids (r + a <= 16):
O((r + a) n) memmoves. On a 1M list, replacing 500k items in one splice
takes 57 ms. An append costs 7 µs with a fresh id above every existing
one (the bridge's increasing ids) and 140 µs with a low id (a memmove
of the index). Before the append path, one append to a 1M list took
3.8 ms (a full Fenwick rebuild). Estimates against measured
heights: mean error 2.4%, p95 25% (a wrap boundary costs a line); 2.9%
and 25% on the pinned fonts at step 3a. After a jump to 61% of a 100k
list the top item holds its place and the item at the viewport bottom
moves 14 pt once, when the rows measure (0.2 pt on the pinned fonts at
step 3a).

The list incremental-equals-rebuild test found one bug on its first
run: a list whose estimates were stale at an unchanged width summed
fresh estimates for measured items too, so a size probe disagreed with
the final layout. Measurements now hold at their own width.

The first JS list example found a bug from milestone 1: the encoder
sent unset margin sides as `auto` instead of 0, so a bubble with only
`margin.left` was pushed to the right. Unset sides are now 0.

Review round 1 of step 2 (GPT-6 Astra): six majors and four minors,
all valid, all fixed with regression tests and negative controls:

- Validation kept a list's item count across removal and re-creation
  of its id in one batch; a stale splice then panicked mid-apply. The
  count resets on create and remove.
- The focused row was kept by index, so a splice above it unmounted it
  (and its input). Items now have identity on the wire; the range event
  names the kept item's identity and the list revision; React keeps the
  row by key and drops ranges from an older item order.
- A replacement splice moved the anchor to the splice start even when
  the anchor item survived (reorders). Identity moves it to its item's
  new place; unchanged moved items keep their measurements.
- Size probes wrapped rows at the border-box width; the final layout
  at the content width. Both use the content width now; the oracle
  covers padding, border, gap, max width, and a following sibling.
- The range ignored transforms between the list and its viewport; it
  maps through them now (tested by hit testing).
- The row gap was ignored; it enters offsets, totals, lookups, and
  anchors.
- A fallback change left unmeasured estimates stale; an f32 delta in
  `Extents::measure` rounded wide changes; rows hidden by index checks
  stayed in the accessibility tree; the list allocation test stopped
  before the renderer. All fixed; the full-frame list test then found
  that dirty queues swapped buffers with their drains, so a warm scroll
  could still grow one: drains copy now.

Review round 2 of step 2: two new majors, two partly resolved, two
minors, all fixed with regression tests and negative controls:

- Identities were not checked for uniqueness; a duplicate made a move
  copy the wrong measurement. Validation now checks every identity
  against the list as the batch leaves it (a persistent index for the
  first splice, the materialized sequence for later ones).
- Equal estimate inputs were taken as proof that a measurement still
  held (a regression from the round-1 fix): an edit with the same
  length kept a stale height. The bridge now marks a move `unchanged`
  (the same object); only that keeps a measurement.
- A flipped list anchored its bottom visible item: the anchor is now
  the visually top item's visually top edge.
- Percentage row gaps resolved to zero; they follow flex now. The
  plain column sums gaps in f32: the definite-height case uses a gap
  f32 holds exactly, so the oracle's tolerance stays.
- Size probes subtracted in f32; they sum in f64 now. The UI-phase
  list allocation test warms its scroll path and runs without a GPU.

Review round 3 of step 2: two majors, three minors, all fixed:

- The multiplicative id hasher clustered chosen ids (see the table
  above); the index is a sorted vector and validation sorts.
- A deferred percentage gap resolved against the raw row sum, not the
  min/max-clamped height (root lists).
- The seeded list test now checks the identity index against the items
  after every step (with multi-splice batches and moves); unknown item
  flag bits reject.
- Numerical equivalence with a fractional gap: both paths are checked
  against an f64 reference within bounds derived from their additions.
  At 200 rows the plain column drifts 0.026 pt (bound 0.48), the list
  0.00003 pt (bound under 1/8 device px at 2x).

### E10: span pool versus `Vec` side table

`cargo run --release -p craie-harness --example e10_span_pool`,
100,000 nodes. Bytes include the per-node slot (a `Vec` is 24 bytes,
a span 8).

| workload                  | Vec: B/node, allocs | pool: B/node, allocs | walk (Vec / pool) |
|---------------------------|---------------------|----------------------|-------------------|
| wide (one parent)         | 29.2, 17            | 18.5, 39             | 0.23 / 0.32 ms    |
| deep (a chain)            | 40.0, 100,000       | 13.2, 17             | 0.41 / 0.25 ms    |
| tiny containers (0-3)     | 32.0, 49,972        | 13.2, 20             | 0.99 / 0.90 ms    |
| huge lists (10 x 10k)     | 30.6, 134           | 18.5, 35             | 0.17 / 0.16 ms    |
| reorder churn (200k moves)| 26.0, 4,510         | 10.7, 56             | 0.51 / 0.17 ms    |

The pool uses 37 to 67 percent fewer bytes and up to 5,800 times fewer
allocations; traversal is equal or faster except on one very wide
list. The pool ships for child lists and chunk ranges.

### Harness findings

The incremental-equals-clean-rebuild suite found two bugs on its first
runs, both fixed:

- A scene whose world matrices were derived before `prepare` kept a
  stale draw list: chunks that had scrolled away stayed in it. The list
  now keys on the world and clip revisions.
- Scroll offsets did not re-clamp when content shrank below them.

The atlas-pressure case showed that pinning the previous frame's
visible set grew the atlas past its budget during scrolls; pinning now
happens at use and before drawing. A real-GPU check measured the
uploads: unchanged frame 0 bytes, fill change 4 bytes (a whole paint
span before `SpanPool::set_at`), scroll 32 bytes.

### Review round 1 (GPT-6 Astra)

Ten findings and two spec deviations, all valid, all fixed with
regression tests. The notable ones: at 1x the window root's identity
matrix never reached the GPU (the GPU tests ran at 2x); native input
(wheel, typing, assistive actions) did not reach scene preparation
without a JS commit, a defect carried from the base; `setText` alone did
not wake the redraw gate; fixed-size text redrew its old paragraph;
TextInput still applied later `value` props (§5 says uncontrolled);
views defaulted to rows (§4 says React Native columns). Snapping became
a per-record policy so transformed content moves by fractions. The
allocation test measures 0 allocations for an unchanged frame and 1 for
a color or transform patch.

### Review round 2 (GPT-6 Astra)

Six findings and two spec deviations, all valid, all fixed with
regression tests:

- A glyph larger than an atlas page no longer disappears: it is
  rasterized at a smaller size and drawn scaled up (a CPU test and a
  GPU pixel test). Font sizes above 2048 points reject.
- Accessibility bounds and the IME area publish only after
  `prepare_frame`, so native reflow publishes final layout.
- The CPU resolver rounds half to even, like WGSL `round`; a GPU test
  checks half-pixel edges at 2x.
- Scroll content has no snap: fractional offsets move it by fractions
  at 1x and 2x. (Partial: it stayed unsnapped at rest; round 3.)
- Editor operations count shapes (a text signature per operation); a
  typing test is the positive control. (Partial: composition-only
  reshapes were missed; round 3.)
- Unknown paint and spatial mask bits reject the transaction.
- Allocations are measured over the whole frame on a real device, per
  phase. Three frame-path allocations were removed: the paint and
  spatial id lists and the built-chunk list (now reused), the renderer's
  per-draw plan vectors and composite bind groups (now kept), and the
  dirty-range vectors (now copied into one reused buffer). An unchanged
  frame and a warm color or transform patch now allocate 0 times in
  Craie code (was 1 for a patch). wgpu's own encode cost on Metal: 56
  allocations per frame, 23 per extra pass, 8 + 5 per buffer write.
  (Partial: range normalization still allocated, hidden inside the
  write allowance; round 3.)
- The copy counter includes span lists (16 bytes per span).

Captures of the JS examples (`CRAIE_CAPTURE`) found one more deviation
from §4: children shrank by default (Taffy's CSS default), so the todo
checkbox shrank next to wrapped text. `flexShrink` now defaults to 0 as
in React Native; the todo text column sets `flexShrink: 1`. The todo,
widgets, and demo screens were checked by eye after the change. The
Rust app and text screenshots are pixel-identical to the round-1
references.

### Review round 3 (GPT-6 Astra): satisfied, three minors

Fixed as the first commits of step 2:

- Snap at rest. A settle signal from the UI clock: a snap-at-rest
  record that has not moved for 0.1 s snaps, and the host wakes at
  `Ui::next_settle` to paint it. Tests: moving and settled output at
  1x and 2x (and under a transformed ancestor) in the resolver, crisp
  edges on the GPU after settling, the frame path's one-row upload,
  and the incremental harness with a moving clock, compared at rest.
- Editor reshapes count where Parley shapes, not from buffer hashes.
  Repeated preedits that only move the caret, finishing a composition,
  and an empty preedit each count one; clean navigation counts zero.
  An oracle outside the counter (a reshape moves the layout's shaped
  data) agrees on every case. An empty preedit, which platforms send
  before a commit, used to reach Parley's non-empty assertion.
- Dirty ranges merge in place (unstable sort, compaction). Prepare
  splits into `collect` (Craie, 0 allocations on patch and idle frames
  in alternation) and `upload` (wgpu).
- A test matrix for React Native flex defaults (grow, shrink, basis)
  across an omitted style, a partial wire style, and a NIL reset.

### Visual checks

`cargo run --example text -- --screenshot` and `--example app` render
through the new renderer. The app demo adds a rotated bordered card, an
isolated opacity group whose squares overlap without darkening, a
rounded clip with scrolled content (scrolling rebuilt no chunk), and a
payload-fed bars surface. Against the baseline image, only centered
labels differ: bounding boxes within one device pixel, from chunk-origin
snapping.

## Pass 3 — interaction, extension, accessibility

Decisions that shaped the interactive layer:

- **One TSFN return channel.** `subscribe(cb)` delivers `ack | events`
  frames — no blocking receive task, no polling. Gotchas that cost
  real debugging: napi's `CalleeHandled` default makes the callback
  `(error, frame)` not `(frame)`, and `Weak` TSFNs don't keep a Node
  worker's event loop alive (worker exits mid-attach). Strong TSFN,
  released on `Session::close`.
- **Node, not Bun, for apps.** Bun hangs requiring N-API addons inside
  `worker_thread`s. Examples bundle via esbuild and run under Node;
  `attachApp` rebinds console to direct fds because Node pipes worker
  stdout through the (blocked) main thread.
- **Listener-relative pointer coordinates.** Coordinates are computed
  per listener on the propagation path, not once for the deepest hit —
  otherwise a slider track receiving a knob's event reads
  knob-relative x. Pointer capture holds the drag on the pressed node.
- **PlainEditor per INPUT.** Parley 0.11's editor gives caret,
  selection, IME composition, and AccessKit hooks for free; Craie adds
  undo rings, arboard clipboard, and the wire commands.
- **Custom = payload + painter, not a framework.** A CUSTOM node is a
  tag + f32x4 + text; registered painters emit quads into the same
  clipped scene. JS painters call back synchronously during paint.
  Extension without a second retained tree.
- **martensite-accesskit-winit 0.18.** accesskit_winit 0.34 targets
  winit 0.30; the martensite fork is patched for 0.31-beta.3. The
  semantic tree is projected from retained state on change (full
  `TreeUpdate` — trees are small); AT actions queue through the wake
  channel onto the UI thread.
- **Atlas LRU + page cap.** Bounded residency: allocation failure
  evicts least-recently-used etagere allocations and marks cache
  entries absent for re-rasterization.

Pass-3 bench deltas (same 5k-row scene): paint+emit cold 2.39 ms, warm
0.05 ms; 500-dirty relayout 19.3 ms. See `docs/PASS3.md`.

## 5,000-row benchmark (`examples/bench`, release build)

A root column with 5,000 rows, each a styled view containing one text
node (10,001 nodes total). Numbers on this machine (Apple M5 Max,
1600x1000 @2x), phases measured independently:

| phase                        | time      | allocs  | notes |
|------------------------------|-----------|---------|-------|
| encode txn (Rust side)       | 11.3 ms   | 10,031  | JS-side analogue |
| wire bytes                   | 683 KB    |         | ~68 B/node incl. strings |
| decode + apply               | 0.54 ms   | 10,095  | ~54 ns/node — strings + rows |
| layout, cold                 | 216.6 ms  | 476k    | Taffy flex + Parley shapes 5,000 leaves |
| — compute vs rounding        | 215.8 / 0.07 ms |  | rounding is free |
| paint + emit, cold           | 1.81 ms   | 263     | 1,101 instances after viewport culling |
| paint, warm unchanged        | 0.04 ms   | 12      | replays retained EmittedText batches |
| layout, warm unchanged       | 0.06 ms   | 0       | Taffy cache absorbs the whole tree |
| Taffy cache                  | 10,001 hits / 75,001 misses | | cold-run counters |
| apply 500 `set_text`         | 0.02 ms   | 17      | |
| layout, 500 dirty            | 10.7 ms   | 44.5k   | 500 re-measures + root relayout |
| paint, 500 dirty             | 0.19 ms   | 33      | only dirty text re-emits |
| commit copy + drain          | 0.01 ms   | 3       | 683 KB txn through Session |
| atlas upload                 | 0.14 ms   | 47      | 105,728 bytes |
| draw submit, first           | 0.66 ms   | 85      | buffer upload + 1 draw call |
| GPU completion, first        | 21.8 ms   | 3       | first-submit warmup |
| draw submit, warm            | 0.18 ms   | 51      | |
| GPU completion, warm         | 0.70 ms   | 0       | |

Retained state: 10,001 nodes x 20 B = 195 KiB of headers; live heap ~35
MiB, dominated by 5,000 retained Parley `Layout`s (shaping output is the
heavy retained state, not our rows).

## Marbre-like transcript (2,000 messages + 300 stream ticks)

A chat-shaped tree — sidebar, 2,000 message rows with mixed text, a
composer — followed by 300 single-message-append transactions:

| phase                        | time      | allocs  |
|------------------------------|-----------|---------|
| mount txn                    | 372 KB    |         |
| decode + apply               | 0.54 ms   | 8,595   |
| layout, cold                 | 125.5 ms  | 313k    |
| paint + emit, cold           | 1.32 ms   | 306     |
| paint, warm unchanged        | 0.03 ms   | 10      |
| 300 stream txns total        | 203 ms    | 74k     |
| — per-txn avg: apply         | 0.000 ms  |         |
| — per-txn avg: layout        | 0.645 ms  |         |
| — per-txn avg: paint         | 0.031 ms  |         |

What this says:

- **The wire path is free.** Decode+apply is 0.5 ms for 10k nodes; the
  in-process session copy is 0.01 ms for a 683 KB commit. A bounded
  queue beats a shared-memory ring at these sizes — no further transport
  work is justified.
- **Taffy layout is the whole story.** Cold layout is ~120–220 ms for
  10k nodes; incremental layout is ~0.65 ms per streaming append. Paint,
  emit, upload, and GPU completion are all noise next to it. Any future
  optimization effort goes to layout (or to not laying out — see below).
- **Warm frames cost nothing.** An unchanged repaint replays retained
  `EmittedText` batches: 0.03–0.04 ms, ~10 allocs, zero shaping,
  rasterization, or glyph-cache lookups. Taffy's cache makes warm
  layout 0.06 ms. Keeping the cache is justified; replacing it buys
  nothing.
- **Retained Parley layouts are the memory floor.** ~35 MiB live for
  5k–10k short rows. The 20 B header is 0.5% of that. If text memory
  matters, the fix is windowing shaped layouts to the visible range —
  not shrinking our own storage.
- **Viewport culling already does the heavy lifting.** 1,101 instances
  reach the GPU from a 10k-node tree. The remaining cost is that
  *layout still visits every node*; a virtualized list or subtree
  damage tracking is the next lever when transcript-scale UIs land.
- **encode 11 ms for 10k nodes in Rust**; the TS encoder differs but
  the op count is the same.

## Frame-cost comparison (`examples/framebench`)

The craie counterpart of gpui-react's `fixtures/performance`: same
scene (800x600, 32-px status line, N identical 20-px text rows, `flow`
retained shape), same protocol (mount, first draw, 10 warmup + 100
measured iterations of a one-op status update + draw, then scroll
steps, then removal + empty draw, live bytes per phase). Transactions
are real React commits — `examples/js/dump-framebench.tsx` renders the
scene through `createRoot`/`renderSync` and captures the sealed frames;
`framebench` replays them. `scripts/measure-framebench.sh` sweeps
100/1k/5k rows x 3 reps.

Two substitutions, matching what Craie can do today:

- **No `list` scene.** List virtualization is planned work; only the
  retained-everything `flow` comparison exists.
- **`scrollAndDraw` replaces `wheelAndDraw`.** Craie has no input
  events or native scroller yet (both planned), so a scroll step
  applies a React-driven `marginTop` change on the content view — the
  mechanism a craie app ships today. After the first step defines the
  style, each scroll txn is a single `set_style` op, exactly what the
  reconciler emits.

Medians on this machine (GPU submit included in `draw`, completion
excluded — same boundary as the fixture):

| phase        | 100 rows | 1,000 rows | 5,000 rows |
|--------------|----------|------------|------------|
| mount        | 0.02 ms  | 0.05 ms    | 0.23 ms    |
| firstDraw    | 8.2 ms   | 8.4 ms     | 37 ms      |
| update.apply | ~0 ms    | ~0 ms      | ~0 ms      |
| update.draw  | 0.16 ms  | 0.15 ms    | 0.13 ms    |
| scroll.apply | ~0 ms    | ~0 ms      | ~0 ms      |
| scroll.draw  | 0.12 ms  | 0.30 ms    | 1.15 ms    |
| remove       | 0.03 ms  | 0.18 ms    | 1.3 ms     |

Live bytes (5k rows): +962 KiB at mount, +26.6 MiB at first draw
(retained Parley layouts), ~+350 KiB over the update phase, −16 MiB
freed at removal.

Note the row shape differs from the synthetic bench above: rows are
bare fixed-height texts (5004 nodes), so Taffy skips the measure
callback and Parley shaping lands entirely in `firstDraw`.

## Text stack: Parley + Swash + etagere

Chosen over GPUI's cosmic-text-style approach because the split matches
what Craie owns: Parley does shaping, bidi, and line breaking; Swash does
rasterization only; Craie owns the cache key and the atlas. The seams are
clean. Parley's `GlyphRun` exposes everything the cache key needs
(`run.font()`, `font_size()`, `normalized_coords()`, `synthesis()`), and
font identity is `FontData.data.id()` + face index, so interning is cheap.

Measured on the `text` example (1800x1200 @2x, multilingual + emoji +
styled runs): 41 glyph runs, 443 glyph instances, 300 rasterizations on
first emit. The cache absorbs the rest. Re-emit after reflow rasterizes
nothing new.

Shaped Parley layouts are retained per text node (`MeasuredText`, 328 B
slot + heap). A dirty text re-measures inside the leaf callback; a clean
text in a relayout re-emits without reshaping. On top of that, each node
keeps an `EmittedText` batch — physical-pixel instances keyed on
(scale, origin, color) — so an unchanged repaint does zero text work,
and a color-only change rewrites instance colors in place. Color is not
part of the layout key; `set_color` marks TEXT-free paint dirty only.

Swash scalers are created lazily — a cache hit never constructs one.

### ICU4X segmentation

Parley warns `No segmentation model for complex script: Chinese/Japanese`
because the ICU4X line-segmentation data is not bundled. CJK glyphs still
shape and render; what degrades is line breaking inside CJK text (breaks
fall back to space-separator rules). Options: enable the `icu` feature on
Parley (pulls compiled segmentation data), or accept degraded CJK
wrapping until the demo needs it. Superseded 2026-09-23: the owned text
stack uses `unicode-linebreak`, which carries the UAX #14 CJK rules, and
Parley leaves production (see `ARCHITECTURE.md` §5).

### Subpixel positioning

The cache quantizes glyph offsets to quarter pixels (2 bits per axis,
baked into `GlyphKey`). Whole-pixel positions share a bitmap; fractional
positions rasterize at the fractional offset. Rounding is `round()` not
`trunc()`, so the bucket 4 case folds into the next integer. Coord
interning is a small linear-scan table — no per-run `Box` allocations.

### Atlas upload cost

Each page tracks a `used` union of everything ever blitted. Fresh
texture arrays (first sync, page-array growth) upload `used` per page —
60 KiB on the wire-demo's first frame, down from 20 MiB of whole pages.
Steady-state updates upload only etagere dirty rects. Measured at bench
scale: 0.14 ms and 105 KB for the whole first frame.

## Renderer

One instanced draw call per frame regardless of node count. Quads and
glyphs share a 40-byte `Instance` row in a single document-ordered
`Vec` — `FLAG_SOLID` marks a rect (no atlas fetch), `FLAG_COLOR` a color
bitmap glyph — uploaded to one growable vertex buffer. Paint order is
vector order; the old quad-then-glyph split (glyphs could never paint
under a later quad) is gone. Nodes outside the viewport emit nothing.

Measured: 0.18 ms warm submit + 0.70 ms warm completion. The 21.8 ms
first-frame GPU completion is Metal warmup, not steady state. wgpu adds
no measurable overhead at this scene size.

### wgpu 30 API changes that bit

- `Surface::get_current_texture` returns a `CurrentSurfaceTexture` enum
  (Success / Suboptimal / Timeout / Occluded / Outdated / Lost /
  Validation), not `Result`. Present is explicit —
  `queue.present(frame)` after `window.pre_present_notify()`; dropping
  the frame discards it.
- Pipeline layouts take `Option<&BindGroupLayout>` entries and vertex
  buffers take `Option<VertexBufferLayout>`.
- `RenderPassDescriptor` has a `multiview_mask` field.
- `SurfaceConfiguration` has a `color_space` field
  (`SurfaceColorSpace::Auto` keeps the old behavior).
- `InstanceDescriptor` has no `Default`; use
  `new_without_display_handle_from_env()` as the base.
- `BufferSlice::get_mapped_range` returns `Result`.

### winit 0.31 API changes that bit

- `Window` and `ActiveEventLoop` are traits;
  `create_window` returns `Box<dyn Window>`.
- Surface creation moves to `ApplicationHandler::can_create_surfaces`
  (the `resumed` split is gone).
- `WindowEvent::Resized` is `SurfaceResized`.
- Frames come from `RedrawRequested` only; with `ControlFlow::Wait` a
  static app sleeps.

## Retained host

Copied from gpui-react commit `46cb47d` ("schema-driven binary wire and
pack native rows"), which measured ~0.7 ms native mount of 5,000 nodes
and cut the wire from ~825 KB to ~435 KB vs JSON.

`NodeHeader` is 20 bytes: `parent` link, `aux` side-table row, wire
`style` id, `kind` index (+hidden bit), `generation`, dirty flags. Ids
are JS-assigned and index the arena directly; `EMPTY_KIND` marks free
slots; `generation` bumps on reuse so stale references can never reach
the new occupant. Children live in a `Vec<NodeId>` side table (plus a
`roots` list): indexed O(1) access for Taffy, no sibling links, empty
vecs allocate nothing.

Dirty tracking is a queue, not a scan: a LAYOUT transition pushes the
node onto `layout_dirty` once; the layout pass drains it and walks
ancestors. Local updates never touch the other 9,999 nodes.

Measured: decode+apply of the 10k-node mount txn takes 0.54 ms; 500
text updates apply in 0.02 ms.

## Taffy integration

Taffy runs over the host through its low-level traits; it never owns a
node. `Layouts` holds the decoded `taffy::Style`s indexed by wire id —
JS already dedupes styles, so there is no native interning pass — plus
three parallel per-node stores (`Cache`, unrounded `Layout`, final
`LayoutData`).

- The measure callback receives content-box `AvailableSpace` — wrap
  width is `available.width` when definite, no padding subtraction
  needed.
- A leaf whose cache key misses gets re-measured; the retained Parley
  layout is keyed on (text dirty | wrap width bits), so unchanged text
  in a relayout emits without reshaping.
- Dirty = bit on the node + queue push + walk ancestors clearing their
  Taffy caches (a node's cache key doesn't include children). Text
  color does NOT set layout-dirty; text content and font do.
- `layout.location` is relative to the parent's *content* box; paint
  accumulates `parent.content_origin` (border + padding) while walking.
- Cache behavior measured: 10,001 hits / 75,001 misses on the cold run;
  warm unchanged layout is 0.06 ms. Rounding is 0.07 ms of a 216 ms
  pass — `RoundTree` is free. The cache stays.
- All coordinates are logical points; the display scale enters only at
  emit, where glyph origins and raster sizes go physical. A scale change
  invalidates emitted batches, not shaped layouts.

## Wire + JS bridge

- The wire is self-describing via op tags and the style presence mask;
  there is no schema negotiation. The JS encoder and Rust decoder are
  pinned to the same constants by `tests/wire_fixture.rs` (fixture
  generated by `packages/bridge/scripts/gen-fixture.ts`).
- `queueMicrotask` seals one transaction per commit, and a commit maps
  to exactly one `Session::submit` — one copied `Vec<u8>` into a bounded
  queue (256 txns / 4 MiB), one `Wake` to the event loop.
- Ack is a `u64` seq through the session's condvar; `recv_acks` blocks
  the ack task until seqs exist. JS recycles removed ids only after
  their txn's seq acks. No sockets, no shared memory, no polling.
- Commit copy measured: 0.01 ms for a 683 KB transaction. A ring buffer
  would save nothing measurable.
- E2E verified: `bun examples/js/host.ts` runs React in a worker,
  submits through `craie-node` (N-API), wakes the winit loop, applies,
  renders, presents, acks.

## Verification so far

- `cargo test`: 17 lib tests (host, wire, ui, layout) + 1 cross-language
  fixture test.
- `bun test` in `packages/bridge`: 7 tests, 49 assertions (golden wire
  bytes, style mask round-trip, reconciler mount/update ops, ack-gated
  id recycling).
- `cargo run --example text -- --screenshot`: multilingual render incl.
  RTL Arabic, CJK, color emoji — verified visually.
- `cargo run --example app -- --screenshot`: wire-built UI rendered
  through the full apply -> Taffy -> Parley -> GPU path. Ordered paint
  verified (code background under monospace text).
- Ack round-trip verified through the session: `Root.flush()` resolves
  only after the native side applies the transaction.
- Idle is by construction (`Wait` + redraw only on request). The bridge
  wakes the loop only when a commit lands; between ticks nothing runs.
