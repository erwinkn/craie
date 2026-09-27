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
- Partly resolved (step 6a): the layout row is a 136-byte `LayoutRow`
  (was 240). Live heap at 5k rows: 39,493 KiB (38.6 MiB) against 41,157
  KiB just before step 6a and 43.8 MiB at step 1; 2.2 MiB above the
  pre-step-1 baseline.
- Step 6c: production runs the owned engine; live heap at 5k rows
  unchanged (39,494 KiB). The per-node cache and output tables are
  still Taffy's `Cache` and `Layout`.
- Resolves in: slimmer per-node cache and output tables in the owned
  engine (later kernel work), or accepted at this size.
- Re-test: `cargo run --release -p craie-platform-winit --example
  bench` (live heap, 5k rows), EXPERIMENTS.md "Step 1".

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

### DF-9: `setText` leaves the caret at the start

- Source: claims and keys (work item 1) implementation (own finding).
- Where: crates/ui/src/input.rs (`Inputs::set_text`).
- Claim: after JS sets an input's value, the caret sits at offset 0,
  so the next keystroke types before the text. A DOM input moves the
  caret to the end when its value is set.
- Why deferred: pre-existing, and controlled values become rebased
  writes (topic 11), which define where the caret goes.
- Resolves in: topic 11's rebased writes, or a one-line change to put
  the caret at the end if a consumer hits it first.

### DF-10: no claims on a nested Text

- Source: claims and keys (work item 1) implementation (own finding).
- Where: packages/bridge/src/host.ts (`emitProps`, nested Text).
- Claim: `onContextMenu`, `onDrop` and `keymap` on a Text nested in
  another Text do nothing: a nested Text is spans of its root and has
  no native node to hold a claim set.
- Why deferred: needs claims per span, which interactive spans
  (topic 11) define; a nested Text cannot take focus, so only the
  pointer claims are affected.
- Resolves in: interactive spans (topic 11).

### DF-11: a paste answer has no range

- Source: claims and keys (work item 1) implementation (own finding).
- Where: crates/ui/src/ui.rs (`Command::InsertText`).
- Claim: `InsertText` replaces the focused input's selection when the
  answer arrives. If the user moves the caret between the claim and
  the answer (one round trip, 0.2 to 5 ms at p99 outside major
  collections, E19), the text lands at the new caret.
- Why deferred: topic 1's target expresses the range against the
  buffer revision the claim saw; revisions and rebasing come with
  rebased writes (topic 11).
- Resolves in: topic 11's rebased writes.

### DF-12: file drops have no position

- Source: PR #4 review (claims and keys).
- Where: crates/platform-winit/src/winit.rs (`HoveredFile`,
  `DroppedFile`).
- Claim: winit 0.30's drop events carry no position, and no cursor
  moves arrive while another app's drag is over the window. The
  driver marks the pointer unknown when a drag enters, and the drop
  goes to the focus path instead of the node under it. Paths that are
  not UTF-8 arrive lossily converted, and Wayland gets no drop events
  at all in winit 0.30.
- Why deferred: the position needs the platform (on macOS,
  `NSEvent.mouseLocation` when the drop lands) or winit 0.31's
  `DragDropped`, which carries it.
- Resolves in: the winit 0.31 upgrade, or a macOS-only position read
  if a drop target needs it first.

### DF-17: Tab and accessibility reach layers after the app

- Source: sibling z and layers (work item 4) implementation (own
  finding).
- Where: crates/ui/src/dispatch.rs (`focusables`), crates/ui/src/a11y.rs.
- Claim: a layer container is a root-level node after the app's roots,
  and Tab and the accessibility tree keep tree order. So Tab reaches a
  menu opened from a toolbar button only after every focusable node of
  the app, and a screen reader reads layers last, in open order.
- Why deferred: where focus goes into and out of a layer is work item
  3's (focus traps, `modal`, owners' scopes); reading a layer next to
  its owner is the accessibility pass's (topic 13).
- Resolves in: work item 3, then topic 13.

### DF-18: a z change walks the whole tree for the draw order

- Source: sibling z (work item 4) measurement (`zorder` example).
- Where: crates/ui/src/scene_sync.rs (`walk_tree`).
- Claim: a z change bumps `structure_rev`, like an insert, and the next
  frame rebuilds the draw order with one walk of the whole tree. At
  100k nodes that frame took 6.0 to 8.8 ms against 2.5 to 3.7 ms after
  a transform change (exe1, loaded); the re-sort itself took 28 to 74
  µs. At 5k nodes: 0.31 to 0.45 ms against 0.17 to 0.29 ms.
- Why deferred: it is the cost every structure change already pays,
  and the draw order is a derived cache by decision (§8). Patching one
  parent's range needs the draw list ranged per parent.
- Resolves in: an incremental draw-order patch for structure changes,
  if reordering or inserting in large trees shows up in a frame
  profile (it would serve inserts and moves too).

### DF-19: layers owned coarsely

- Source: sibling z and layers (work item 4) implementation (own
  finding).
- Where: packages/bridge/src/index.ts (`Layer`).
- Claim: a `Layer`'s owner is the enclosing `Layer`'s container, and a
  top-level `Layer` has none. So an unowned layer with a negative z
  sorts under the app (the kit's layer tokens are all positive), and
  owners know nothing finer than a layer (a trap inside it).
- Why deferred: the native op takes any node as owner; finding a finer
  one (the trap, or the host node that opened the layer) is work item
  3's, with focus traps.
- Resolves in: work item 3.

### DF-20: an owner-only layer closes when Suspense hides it

- Source: sibling z and layers (work item 4) implementation (own
  finding).
- Where: packages/bridge/src/index.ts (`Layer`).
- Claim: a `Layer` with no children of its own, opened only to own a
  nested one, closes in its layout-effect cleanup. A Suspense boundary
  that hides it runs that cleanup and keeps the nested layer, which
  loses its owner and sorts at its own z after the reveal. (A layer
  with children stays open: it closes with its last child.)
- Why deferred: no kit component nests a layer in a childless one. The
  fix reopens the container on setup and hands the nested layers their
  owner again.
- Resolves in: work item 3, with owners.

## Closed

- DF-8 (2026-09-24, same day): `native_reflow_publishes_after_the_frame`
  failed; I first recorded it as caused by the machine (the display had
  locked). Review of the Pulse demo (SPD-08) found the real cause: the
  test measures text but had no fonts of its own. In whole-workspace
  runs the harness's `pinned-fonts` feature reached it; alone, or once
  the new headless test installed the system fonts globally in
  parallel, it had none or the wrong ones. It uses the pinned fonts
  explicitly now (`craie-text` dev-dependency with `pinned-fonts`) and
  passes alone and in the full binary.

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
- S5B-16 (size limits: percentages against the containing block, box
  sizing): fixed after step 5b round 3 (the last); reviewed with the
  step 6 range, which starts at its commit.
- PR1-01..05 (the release graph checks both Apple targets and says
  Linux is not shipped, and its failure names `cargo fetch`; the
  last-resort font test covers the by-name tie-break on every platform;
  comment accuracy): fixed in the PR #1 review (Linux-host test fixes).
- PR2-01..09 (E19 review): the react phase ends at the layout effect,
  so encode and send count as apply (relabelled; a direct `js` phase,
  native to committed, replaces summed p99s); a batch of presses now
  renders between presses (`flushSyncWork` after each discrete event,
  tested with a toggle pressed twice in one batch); windowed clicks are
  woken by a thread at their due time, not a loop timer that can fire
  late; the windowed frame ends when `present` returns, and the report
  says so; unanswered clicks count as infinite round trips and stale CSVs
  are removed before each load; nearest-rank percentiles use `ceil`; GC
  overlap counts pauses during [dispatched, committed]; the probe allows
  30 s before the marker shows. Fixed in the PR #2 review. PR2-10
  (tests for continuous priority and a throwing handler) left: in Node a
  continuous update and a default one both commit in a later scheduler
  task, so a test cannot tell them apart without React's internals.
- PR3-01..10 (E15 review), fixed in the PR #3 review:
  - PR3-01: upkeep is 2 to 7 percent of the layout pass that stales the
    index (9.1 of 450 µs to 297 of 4,172 µs), not 3 to 30, in
    EXPERIMENTS, ARCHITECTURE-update and the run log.
  - PR3-02: the index table has the walk from its own run; the older
    walk-only table is labelled as such; every quoted ratio comes from
    one run.
  - PR3-03: §13's decision reads "no separate spatial index (R-tree,
    rebuilt BVH): the node tree carries a bounding box per subtree"
    in ARCHITECTURE and its update table.
  - PR3-04: the randomized test reuses freed ids at once and randomizes
    borders and padding (fixed and percentage); a new test covers a
    clip that moves inside an unchanged box, the one case the layout
    pass's clip term catches alone. Deleting each of the six hooks in
    turn fails at least one reach test. `create`'s stale bit was
    redundant (inserting touches) and is gone. Borders found an older
    crash: `in_rounded` used `f32::clamp` with bounds that rounding can
    cross.
  - PR3-05: `pointer_enter_leave_sequences` asserts the exact enter and
    leave sequence across siblings, cousins, separate roots, a removed
    parent, a detached grandparent and a removed hovered node.
  - PR3-06: `Ui::render` refreshes the index once layout is done;
    `dispatch` keeps its refresh as a backstop; the probe's hit test
    after a frame is pruned too.
  - PR3-07: fixed rather than softened, as far as tests reach: the pad
    scales with the transform's condition number (‖A‖²/2|det A|), and a
    transform with no inverse has an empty reach. A new test aims a
    million points at the edges and corners of boxes squashed up to
    100,000-fold; the old pad fails it. The docs no longer say "never":
    `reach.rs` calls the pad an error estimate, not a proof.
  - PR3-08: the title and docs say walking a propagation path no longer
    allocates; the events themselves still do.
  - PR3-09: two reruns put wide 10k's refresh after one transform at
    12.9 and 11.9 µs: the 92 was noise, now marked in the table.
  - PR3-10: removing or detaching the hovered node or an ancestor hands
    the hover to the subtree's parent, so ancestors get no second enter.
- PR4-01..13 (claims review), fixed in the PR #4 review:
  - PR4-01: the bridge tests expected Ctrl for `mod` and failed on
    macOS; they follow the platform now, and chord parsing is tested
    for both.
  - PR4-02: drops went to the last cursor position before the drag,
    which can be anywhere; see DF-12. Paths are joined by NUL, not a
    newline, which a path can hold.
  - PR4-03: `parseChord("constructor")` read `Object.prototype`
    (`Object.hasOwn` now).
  - PR4-04: the window list puts the latest mounted `useHotkeys` first,
    so an overlay's Escape beats the page's; a deviation from Marbre,
    where both fire, recorded in topic 1.
  - PR4-05: `enter` submits on exactly Enter, as in Marbre (Shift+Enter
    submitted a single-line input); `SubmitKey` includes `none`; an
    unknown `submitKey` logs once and falls back to `enter` instead of
    throwing in the commit.
  - PR4-06: `shift+1` never matching on a US layout is parity with
    Marbre's `matchesChord`, now documented in `parseChord` and topic 2.
  - PR4-07: an unknown chord logs once per chord, not on every render.
  - PR4-08: on macOS, Ctrl+click is a secondary press (context menus);
    the release matches its press.
  - PR4-09..13 (nits): the handler refresh is commented; the ack readers
    that skip event order say so; with no ack transport a replaced
    version's handlers go at once instead of piling up; the unused
    `Mods::command` is gone; `KeyEvt.code` is the web's `event.code`.
  - Tests added: CLAIMS and INPUT_CONFIG decode of malformed bytes
    (a NUL character key is now invalid), the platform's key
    translation (`us_char`, F13 to F24), the drop fallback, the window
    list's old versions pruned on ack, and submit key `none`.
