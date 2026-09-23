# Ledger

Everything accepted or deferred on the way to the target architecture,
so that nothing is dropped without a record. The end-of-vision review
campaign sweeps this file. Update it in every range: add an entry when
a degradation is accepted or a finding is deferred, and move an entry
to "Closed" (with the commit) when it is resolved.

## Accepted regressions

Measured degradations accepted while an area has not reached its
target shape.

### AR-1: per-node layout rows (`taffy::Style`)

- What: live heap per node. Dense host rows cost 348 bytes per node
  (the layout row, a `taffy::Style`, is 240 of them) against about 70
  bytes before step 1.
- Baseline vs now: live heap at 5k rows (`examples/bench`) 36.4 MiB at
  `f26c22b` vs 43.8 MiB at step 1 (+7.4 MiB; struct sizes explain
  about 5.3 MiB).
- Why accepted: layout inputs are per-node rows by decision (step 1);
  Taffy needs its own `Style` per node until Craie owns flex layout.
- Resolves in: step 6 (owned flex layout: a compact owned layout row).
- Re-test: `cargo run --release --example bench` (live heap, 5k rows),
  EXPERIMENTS.md "Step 1".

### AR-2: long-Latin rewrap time against Parley

- What: a width change (rewrap) on long Latin paragraphs.
- Baseline vs now: Latin 386 bytes at 120 pt, 3.88 µs (Craie) vs 2.62
  µs (Parley), 1.48x; latin-narrow 1.17x, styled 1.21x. Other E01 cases
  0.69x to 1.13x. Both sides allocate nothing once the stores have
  grown. Craie holds 0.29x to 0.75x Parley's bytes and lays out cold in
  0.58x to 0.93x its time.
- Why accepted: Craie rebuilds its cluster scratch from the glyph store
  on each rewrap, and placement walks cluster groups (so an L1-reversed
  segment places correctly); Parley keeps a cluster table. Astra ruled
  it non-blocking (step 3a, round 2).
- Resolves in: an E01 follow-up measurement of cluster reconstruction
  against placement traversal (retained cluster table vs rebuild), then
  E04 (incremental reflow).
- Re-test: `cargo run --release -p craie-harness --example e01_text`
  (rewrap column).

### AR-3: allocations of a cold bidi layout

- What: allocations per cold layout of a paragraph that needs the bidi
  algorithm.
- Baseline vs now: bidi-ltr 24 (Craie) vs 21 (Parley), bidi-rtl 30 vs
  29, hebrew-lines 32 vs 24, styled-synth 20 vs 16. Every other E01 case
  allocates fewer than Parley (6 to 15 vs 16 to 33). Editing (step 3b):
  a keystroke pair on the 55-byte bidi text allocates 60 vs Parley's 43;
  Latin and multilingual allocate fewer (30 vs 41, 24 vs 53).
- Why accepted: `unicode_bidi::BidiInfo` allocates its level and class
  tables per paragraph; LTR-only text skips it.
- Resolves in: E02 (generated Unicode tables versus the unicode-*
  crates), or bidi scratch reused in the shaper.
- Re-test: `cargo run --release -p craie-harness --example e01_text`
  (cold allocs column; `editing` rows, pair allocs).

### AR-4: SF raster count against Helvetica (a decision, not a regression)

- What: the Text default family is `system-ui` (SF on macOS), not
  step 2's `sans-serif` (Helvetica): ARCHITECTURE.md §5, Decisions.
- Baseline vs now: 1,000 framebench rows need 110 rasterizations in SF
  and 71 in Helvetica, on both engines. Framebench first draw with SF:
  7.1 / 31.2 / 101.8 ms (100 / 1k / 5k rows) vs 5.7 / 21.2 / 93.4 ms for
  step 2 on Helvetica; on the same font (Helvetica) step 3a is faster
  than step 2 at every size (4.3 / 18.0 / 83.3 ms). Load average 30 to
  85 during the measurement.
- Why accepted: React Native compatibility. The cost is the font's,
  not the engine's.
- Resolves in: not a defect; re-measure when glyph raster work changes
  (E08).
- Re-test: `scripts/measure-framebench.sh` with and without
  `FAMILY=sans-serif`; `cargo run --release --example fontcost --
  system-ui` and `-- sans-serif` (raster counts).

### AR-5: a pass and a composite per run of path meshes

- What: every maximal run of consecutive mesh draws renders into a
  multisampled offscreen layer and composites back (two passes, a
  resolve, and a layer target per run). A UI that interleaves icons with
  text and boxes pays per icon.
- Baseline vs now: before step 5a nothing drew meshes; a frame without
  meshes pays nothing (no layer, no multisampled target). Not yet
  measured on an icon-heavy workload.
- Why accepted: the single-pass alternative (the whole frame at 4x)
  broke analytic anti-aliasing of rects, glyphs, and clips (step 5a
  review rounds 1 to 3); correct painter order wins over pass count
  (section 8).
- Partly resolved (step 5a round 4): content that overlaps none of the
  open run draws before it and the run goes on. Meshes on two sides of
  overlapping content still resolve apart (a seam on a shared edge).
- Resolves in: E07 (coverage/strip preparation needs no
  multisampling at all), or a sample-preserving path target.
- Re-test: `crates/render/tests/paths.rs` (`stats.layers`), and a
  framebench scene with icons once 5b lands.

## Deferred findings

Reviewer minors and nitpicks not fixed yet.

### DF-1: no per-cluster text mapping in accessibility

- Source: step 3a (`75809c2..110795f`), round 2, spec probe (Astra).
- Where: crates/ui/src/a11y.rs (text nodes).
- Claim: the AccessKit projection has no per-cluster text runs
  (character positions, word boundaries), so assistive technology gets
  no caret or selection geometry inside a paragraph.
- Why deferred: not a regression (step 2 had none either). Steps 3b
  and 3c own the positions (the input caret and selection, the
  read-only selection) but project only values. AccessKit text runs
  (per-cluster character lengths, positions, widths, word lengths, and
  the text selection) are an accessibility projection change of their
  own; step 3c's scope was selection, nested Text, and per-span events.
- Resolves in: the accessibility pass over owned text (reads
  `Paragraph::line_clusters` and `Ui::selection_ranges`).

### DF-2: pointer enter and leave on nested Text

- Source: step 3c (`8f2b86e..b6b4dea`), round 1, S3C-10 (Astra).
- Where: packages/bridge/src/host.ts (`POINTER_HANDLER`),
  crates/ui/src/dispatch.rs (hover).
- Claim: `onPointerEnter` and `onPointerLeave` on a nested Text never
  fire: native tracks hover per node, not per span, and sends no span
  on enter and leave; the bridge routes those events to the root.
- Why deferred: minor (reviewer's severity). Press, down, up, and move
  route per span; the root's own enter and leave handlers are
  unaffected.
- Resolves in: a span-boundary hover pass (native tracks the hovered
  span of the hovered text node and emits enter and leave with it),
  when a consumer needs it.

### DF-4: padding and gap tweens need lengths

- Source: step 4 implementation (own finding).
- Where: crates/ui/src/animation.rs (`current_num`, `declared_num`).
- Claim: a padding or gap change from or to a percent jumps instead of
  tweening; width and height resolve percents and `auto` by the probe
  layout.
- Why deferred: resolving a percent padding needs the containing
  block's width per frame; section 12 names `auto` only for sizes.
- Resolves in: the owned flex layout (step 6), which can report
  resolved padding and gap per node.

### DF-5: retargeting restarts the full duration

- Source: step 4 implementation (own finding).
- Where: crates/ui/src/animation.rs (`start_animation`).
- Claim: a transition reversed midway runs its whole duration from the
  value on screen; CSS shortens a reversing transition by the
  proportion already run.
- Why deferred: behavior refinement, no correctness effect.
- Resolves in: when a consumer reports it.

### DF-6: SVG features the importer does not represent

- Source: step 5b implementation (own finding).
- Where: tools/svg-import/src/lib.rs (`Report`).
- Claim: clip paths and masks (drawn unclipped), filters, blend modes,
  patterns, images and `foreignObject`, text (outline it first; found
  in the source, since usvg drops it without fonts), stroke dashes
  (drawn solid), `miter-clip` joins (drawn as miter), `vector-effect`
  (ignored), spreads other than pad, radial focal points (drawn
  centered), gradients past 64 stops (truncated), and group opacity
  over two or more painted items (folded into each; a fill and its own
  stroke count) are not represented. The importer reports each one and
  fails unless `--lenient`.
- Why deferred: section 9's initial profile names clips, images, and
  group opacity; they need scene support (path clips, image resources,
  isolated groups inside a chunk) beyond meshes.
- Resolves in: path clip records in the scene (clips), image resources
  (section 10), and group layers inside vector chunks.

### DF-7: vector detail follows the box, not the world scale

- Source: step 5b implementation (own finding).
- Where: crates/ui/src/vector.rs (`prepare`, the cache key).
- Claim: tessellation tolerance comes from the content box and the
  display scale; a transform that scales a vector node up (an
  animation, a zoom) draws the same meshes larger, so curves can show
  facets past about 4x.
- Why deferred: re-tessellating on every scaled frame costs more than
  the defect; transforms of vectors are rare in the target UIs.
- Resolves in: keying the cache on the world scale in steps (powers of
  two), or E07 (coverage preparation).

## Closed

- S3A-14 (emoji presentation by the Unicode property, VS15/VS16) and
  S3A-15 (exact GB9c and ligature guards) and S3A-16 (framebench
  records family and face): fixed in step 3a round 2, not deferred.
- S3C-01..08 (selection refresh and identity, 2D positions, virtual
  text moves, ligature spacing, span routing revisions, intern keys,
  selection in the rebuild oracle) and S3C-09, S3C-11, S3C-12 (surrogate
  offsets, span limit, span flag bits): fixed in step 3c round 1.
- S3C-13..16 (hidden text roots keep their text, reused slots resolve
  fonts afresh, u32 paragraph revisions, hidden or detached domain
  ancestors) and S3C-17..19 (a press in an input clears, copy keeps
  empty paragraphs, exact distance under shear): fixed in step 3c
  round 2.
- DF-3 (no completion signal for `animate`): promoted into step 4 by
  the parent and done in round 1 (`ANIMATION_END` event, a promise from
  `node.animate`).
- S4-01..07 (probe side effects, dependent probe targets, percent
  padding and gap, recycled layout, unresolved retargets, clamped
  retarget starts, overflow in transforms): fixed in step 4 round 1.
- S4-08..10 (lossless delivery of animation ends, list state through
  the probe, rejected `animate` calls with no side effects): fixed in
  step 4 round 2.
- S4-11 (a resume racing a stalling pump) and S4-12 (`auto` targets of
  list rows): fixed after step 4 round 3, not reviewed again (three
  rounds run).
- S5A-01..06 (rect coverage in multisampled frames, snapped cull and
  layer bounds, gradients without stops, mesh entries in the rebuild
  oracle, malformed meshes, non-finite tessellation output): fixed in
  step 5a round 1.
- S5A-07..12 (glyph and clip coverage in multisampled frames,
  zero-area rects, transform-aware quad margins and layer bounds, paint
  runs and gradient space in the rebuild oracle): fixed in step 5a
  round 2.
- S5A-13..18 (glyph filtering past the gutter, nested clips on paths,
  anisotropic rects, enlarged and reduced glyphs, gradient tolerance in
  the oracle): resolved by the path-layer redesign after step 5a round
  3 (rects and glyphs never render multisampled; per-clip sample tests;
  a dimensionless gradient parameter in the oracle), with regression
  tests; reviewed in round 4.
- S5A-19 (seams where disjoint content split a run) and S5A-20
  (gradient tolerance near hard stops): fixed after step 5a round 4;
  reviewed with the 5b range (which starts at `e9200c7`).
- S5B-01..10 (expanded path bound in asset decoding, pixel-conservative
  run merging, pre-scan for what usvg drops, vector chunks on scale
  changes, content-box intrinsic sizing, device-px tolerance at any
  asset scale, group opacity over painted items, radial oracle through
  the full mapping, stop truncation and miter-clip reports): fixed in
  step 5b round 1.
- S5B-11..15 (viewport clipping, size limits and the aspect ratio,
  stylesheet vector-effect, implicit subpath starts under transforms,
  assets past runtime limits): fixed in step 5b round 2.
