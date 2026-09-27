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
  (drawn solid: `CRV1` has no dash field, though runtime drawings dash
  since work item 8), `miter-clip` joins (drawn as miter), `vector-effect`
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

### DF-13: the dash offset is not animatable

- Source: runtime vector shapes (work item 8) implementation (own
  finding).
- Where: crates/ui/src/animation.rs (`Prop`), crates/ui/src/wire.rs
  (`DRAWING`).
- Claim: ARCHITECTURE-update topic 10 targets an animatable dash offset
  (spinner rings, progress rings). `ANIMATE` (0xA1) drives node
  properties, 0 to 7, and a drawing has many shapes, each with its own
  offset; the offset changes only by sending the drawing again, which
  re-tessellates it.
- Why deferred: animating a shape property needs a shape address in the
  op (node, shape index) and a mesh rebuild per frame, or dashing in the
  shader; neither extends 0xA1 cleanly. A rotating node covers spinners.
- Resolves in: shape-level animation targets, or dashes in the shader
  (E07).

### DF-14: no inherited color in drawings

- Source: runtime vector shapes (work item 8) implementation (own
  finding); narrowed in the PR #5 review (PR5-06).
- Where: packages/bridge/src/shapes.ts (`paint`).
- Claim: `currentColor` resolves to the `color` prop of the `Vector` or
  a `G` above the shape, as `<svg color>` does; it does not inherit a
  color from the node's ancestors (there is none to inherit), and
  without a `color` prop it throws. The kit's token colors (`'ink-3'`)
  must be resolved to colors before they reach a shape.
- Why deferred: the color a node inherits comes with state styles and
  paint sources (topic 5).
- Resolves in: topic 5.

### DF-15: shapes must be direct children of a Vector

- Source: runtime vector shapes (work item 8) implementation (own
  finding).
- Where: packages/bridge/src/shapes.ts (`flattenShapes`).
- Claim: `Vector` reads its children as elements (`Path`, `G`, arrays,
  fragments); a component that returns shapes (`<MyArrow />`) throws.
  The kit's icon registry passes `[tag, attrs]` data, which maps to
  direct elements; the kit's own `Path` and `Circle` are components
  using hooks, and throw.
- Why deferred: rendering shapes through the reconciler would give each
  a host node; flattening at render keeps one op per drawing.
- Resolves in: host shape nodes if a consumer composes shapes from
  components.

### DF-16: group opacity and fill-under-stroke overlap in drawings

- Source: runtime vector shapes (work item 8) implementation (own
  finding); corrected in the PR #5 review (PR5-03, PR5-04).
- Where: packages/bridge/src/shapes.ts, crates/vector/src/svg.rs.
- Claim: two differences from a browser remain. A `G`'s opacity
  multiplies into each shape inside, so overlapping shapes in a faded
  group show their overlap (the importer's DF-6 case). A shape's own
  `opacity` multiplies into its fill and its stroke separately, so a
  faded shape with both shows its fill under the inner half of its
  stroke. `Vector`'s `opacity` is the node's (one layer) and has
  neither problem; dash corners at a closed subpath's start are joined.
- Why deferred: both need an isolated layer per group or shape.
- Resolves in: group layers (DF-6).

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
  owners know nothing finer than a layer (a trap inside it). The other
  way round, a top-level `Layer` defaults to z 0: an app root with a
  positive `zIndex` covers every unowned layer.
- Why deferred: the native op takes any node as owner; finding a finer
  one (the trap, or the host node that opened the layer) is work item
  3's, with focus traps.
- Resolves in: work item 3.

### DF-21: a variant's transform replaces the whole matrix

- Source: work item 5 (state styles).
- Where: crates/ui/src/states.rs (`Values::transform`).
- Claim: a variant carries one 2D affine, as `TRANSFORM` does. With
  `style={{ transform: rotate }}` and `_hover={{ style: { transform:
  scale(1.02) } }}`, hovering drops the rotation. Marbre's `scale`,
  `rotate` and `translateX/Y` are separate keys that compose.
- Why deferred: parts need a transform made of parts on the wire, for
  the base and the animation driver too, not only in variants.
- Resolves in: when a Marbre component overrides one transform part in
  a variant; until then the facade author writes the composed matrix.

### DF-22: transitions and animations inside a variant

- Source: work item 5.
- Where: packages/bridge/src/host.ts (`variantValues`).
- Claim: a variant can change what a property is, not how it moves. A
  `transition` or `animation` inside `_hover` is ignored; the element's
  own `transition` applies to every change, whichever state caused it.
- Why deferred: Marbre's per-state timing is presets (enter, loop),
  which is work item 6.
- Resolves in: work item 6 (motion presets).

### DF-23: text metrics in variants

- Source: work item 5.
- Where: packages/bridge/src/host.ts (`variantValues`).
- Claim: a variant on a Text can set `color` only. A `fontSize`,
  `weight` or `lineHeight` under `_hover` or `_narrow` is logged once
  (`a variant does not apply "fontSize"`) and left out. Box values on a
  Text are left out the same way, since a Text has no box.
- Why deferred: metrics reshape the paragraph, so they belong with the
  text pipeline's own ops, not in the paint-and-layout overlay.
- Resolves in: when a component needs a responsive type size; the
  facade can re-render with a different variant meanwhile.

### DF-24: input color and vector currentColor

- Source: work item 5.
- Where: crates/ui/src/host.rs (`Host::colors`), the text input config,
  the vector path.
- Claim: `COLOR` reaches spans only. A TextInput keeps its config
  color (the facade warns on `color` in its variants). A vector's
  `currentColor` resolves in the facade to the `color` prop of the
  `Vector` or a `G` above (PR #5, DF-14), not to the inherited `COLOR`:
  `<Pressable color="#9aa0aa" _hover={{ color: "#fff" }}>` recolors its
  label but not an icon drawn with `currentColor`.
- Why deferred: the input's color lives in its editor config, and a
  drawing's colors are resolved before they reach native.
- Resolves in: one lookup of `Host::colors` at paint for both, which
  closes DF-14 too.

### DF-25: no platform source for touch and reduced motion

- Source: work item 5.
- Where: crates/platform-winit/src/app.rs (TODO(macOS)),
  crates/ui/src/states.rs (`Ui::set_touch`, `Ui::set_reduced_motion`).
- Claim: the `_touch` and `_reducedMotion` bits exist and resolve, but
  nothing sets them: the driver leaves both false.
- Why deferred: on macOS, reduced motion is
  `NSWorkspace.accessibilityDisplayShouldReduceMotion` plus its change
  notification, and touch stays false on desktop. It needs an objc
  call on the Mac.
- Resolves in: the next macOS pass.

### DF-26: variants on a nested Text

- Source: work item 5.
- Where: packages/bridge/src/host.ts (`emitComposite`).
- Claim: a nested Text is a span, not a node, so it has no table:
  `<Text>see <Text _hover={…}>docs</Text></Text>` logs a warning and
  the span stays as is.
- Why deferred: interactive spans become scopes with topic 11.
- Resolves in: topic 11 (interactive spans).

### DF-27: hover can oscillate

- Source: work item 5.
- Where: crates/ui/src/states.rs (hover at rest).
- Claim: hover resolves again after a restyle with the pointer at
  rest. A variant that moves or shrinks the hovered node away from the
  pointer (`_hover: { style: { marginLeft: 40 } }`) unhovers it, which
  restores it under the pointer, and so on, one frame each.
- Why deferred: the web has the same loop; it's an authoring mistake,
  and nothing in Marbre does it. A guard (freeze hover for a frame
  after it changes layout) would hide real updates.
- Resolves in: if a real component hits it.

### DF-28: animate on a tabled node

- Source: work item 5.
- Where: crates/ui/src/animation.rs (`intercept`), crates/ui/src/states.rs.
- Claim: `ANIMATE` on a node with a variant table sets its base and
  runs. If a variant in effect sets the same property (for layout, the
  same key), the restyle in the same frame declares the variant's
  value, which retargets the animation there, or with no transition
  cancels it: `animate(width, 160)` under `_narrow: { style: { width:
  60 } }` in a narrow window ends at once with reason `cancelled`, and
  the width stays 60. Under `_narrow: { style: { height: 44 } }` it
  runs (before the per-key fix, PR7-02, the variant's paired width cut
  it). Once the variant turns off, the node shows the animated base.
- Why deferred: the owner of a property under a variant is the table;
  explicit animations on the same property are rare, and work item 6
  decides how presets and variants mix.
- Resolves in: work item 6.

### DF-29: kit values variants do not apply

- Source: work item 5; the PR #7 review (PR7-12, PR7-16).
- Where: packages/bridge/src/host.ts (`variantValues`).
- Claim: Marbre's variants also set `z`, `pointerEvents`,
  `visibility`, elevation and the focus ring, and percent translates;
  Craie's apply paint, opacity, a transform matrix and layout only.
  `_hover={{ pointerEvents: "none" }}` or `style: { zIndex: 2 }` in a
  variant is logged once and left out, as is every other key a variant
  does not apply.
- Also: `_hover` on an element that is no scope means the nearest
  scope's hover in Craie, the element's own on Marbre web (`<Text
  _hover>` in questions.tsx, Icon `_hover` in tool-run.tsx). Inside a
  Pressable they agree; a bare `<Text _hover>` outside one logs "needs
  a scope" and does nothing.
- Why deferred: each needs its own native value in the table (z
  re-sorts the parent, pointer events and visibility change hit
  testing, the focus ring and elevation are paint sources), and none
  is on a screen the kit ports first. An implicit scope per `_hover`
  element would make every such node a scope.
- Resolves in: when a ported component needs one; the Marbre port
  flags the `_hover` difference.
### DF-30: images are decoded per node, and fetched per mount

- Source: images (work item 8) implementation (own finding).
- Where: crates/ui/src/image.rs (`Images`), packages/bridge/src/index.ts
  (`Image`, `loadImage`).
- Claim: each node owns its bytes, decode, and raster. A list of 100
  rows showing the same avatar URL fetches it 100 times, holds 100
  copies of the bytes, and decodes 100 bitmaps (vectors intern by
  content; images do not). Each node's encoded bytes are also held
  twice: in the facade's state and in the core.
- Why deferred: sharing needs a key (content hash, or URL from JS) and
  a decode per (source, crop, size) with reference counts; the first
  consumers show distinct photos.
- Resolves in: an image cache keyed by source in the facade (fetch)
  and the core (bytes and bitmaps), when a consumer repeats images.

### DF-31: an image does not clip to its own border radius

- Source: images (work item 8) implementation (own finding).
- Where: crates/ui/src/scene_sync.rs (clip records), crates/ui/src/image.rs
  (`build_image`).
- Claim: `borderRadius` on an `Image` rounds its background and border
  but not the picture: a node's clip applies to its children, not its
  own content. A round avatar clips in a parent:
  `<View style={{ width: 40, height: 40, overflow: "hidden" }}
  borderRadius={20}><Image src={url} style={{ width: "100%", height:
  "100%" }} /></View>` (tested: `a_rounded_parent_clips_an_image`).
- Why deferred: clipping a chunk by its own node's shape needs a clip
  record per rounded image, or a rounded-rect mask in the glyph path.
- Resolves in: content clipping for the node's own chunk (also wanted by
  surfaces), or a corner mask on image quads.

### DF-32: kit `Image` gaps

- Source: images (work item 8), kit API comparison (Marbre
  `ImageProps`).
- Where: packages/bridge/src/index.ts (`Image`).
- Claim: `src`, `fit`, `alt`, `onError` match the kit. Missing: a numeric
  `src` (a bundled asset id); SVG data URLs, which the kit's native
  version renders with `SvgXml` (here the decoder rejects them and
  `onError` fires; SVG goes through `Vector`); `placeholder` and
  `fallback` (compose them in JS: render the fallback after `onError`);
  animated GIFs (the first frame only); `radius` (DF-31); ICO files,
  which the kit's `Favicon` and `SourceChip` load from sites (the
  `image` crate's `ico` feature is off, so they fail with `onError`).
- Why deferred: each needs a decision outside images: an asset
  registry, runtime SVG documents (build-time only, §9), JS composition.
- Resolves in: the kit port, per gap.

### DF-33: one decoder thread, full decode before the downscale

- Source: images (work item 8), measurement.
- Where: crates/platform-winit/src/images.rs.
- Claim: the decoder decodes the whole image, then averages the crop
  down. A 12-megapixel JPEG costs about 63 ms on one core (exe1, loaded;
  50 ms of it the decode) and a transient 36 MB buffer, whatever size it
  shows at. One worker serializes a screen of photos: twenty take over a
  second before the last appears. Probes go first, and queued work for
  a removed or replaced image is dropped (PR8-04), but a decode that
  has started runs to the end.
- Why deferred: the four-format `image` crate has no reduced-size JPEG
  decode (DCT scaling, 1/2 to 1/8); a pool needs a budget for the
  transient buffers.
- Resolves in: scaled JPEG decode (zune-jpeg's or a decoder that has it),
  a small pool sized by the transient-memory budget.

### DF-34: resizes draw the old bitmap, and big images cap at a page

- Source: images (work item 8) implementation (own finding); corrected
  in the PR #8 review (PR8-06, PR8-15).
- Where: crates/ui/src/image.rs (`plan`, `grown`, `Bitmap::stale`).
- Claim: three approximations. While a box grows, the old bitmap draws
  scaled up until the larger decode lands: a decode's time, 60 to 100
  ms for a 12-megapixel photo on exe1, more behind other work. A grow
  asks for 1.25 times the size (capped by the source and a page), so a
  box growing steadily redecodes every 25 %; a box that shrinks keeps
  its bitmap until it is under half the size. The decode size caps at
  an atlas page less its gutter (2,046 px a side), so a full-screen
  image on a 5K display draws scaled up. The downscale averages sRGB
  values, not linear light, so fine high-contrast detail comes out
  slightly dark.
- Why deferred: no consumer shows full-screen images yet; linear
  averaging doubles the downscale cost.
- Resolves in: an image texture outside the atlas for large images
  (DF-36); linear-light filtering if photos look wrong next to a
  browser.

### DF-35: no color management

- Source: PR #8 review (images), finding 15.
- Where: crates/platform-winit/src/images.rs (`decode`).
- Claim: the decoder ignores embedded ICC profiles and treats every
  image as sRGB. A Display-P3 photo (an iPhone's) shows its P3 values
  as sRGB: reds and greens look desaturated next to a browser.
- Why deferred: converting needs the profile read per image and a
  transform per pixel (`moxcms` is in the graph, through `image`); on a
  wide-gamut display, drawing P3 properly also needs a wide-gamut
  surface, which the renderer does not have.
- Resolves in: an ICC-to-sRGB transform in the decoder (cheap after the
  shrink); wide-gamut output with the renderer's color work.

### DF-36: image memory: large images share the glyph atlas

- Source: PR #8 review (images), finding 7.
- Where: crates/ui/src/image.rs (`Images::trim`), crates/scene/src/atlas.rs.
- Claim: the core's copies of decoded pixels are capped (64 MB, least
  recently drawn dropped first, decoded again on a miss; PR8-07), but
  the atlas is not image-aware. A 2,046 px image fills a whole color
  page; the color atlas's cap is 2 pages (32 MiB), so a few large
  visible images pin pages past it (`over_budget_pages`), and a page is
  never freed once made.
- Why deferred: the fix is a texture per large image outside the atlas
  (a new binding in the glyph path, or its own instance type) and page
  reclaim in the atlas; both are renderer work beyond the review.
- Resolves in: large images in their own textures, and atlas page
  reclaim, when a consumer shows full-screen images.

### DF-37: a layout transition redecodes as it goes

- Source: PR #8 review (images), finding 6.
- Where: crates/ui/src/image.rs (`build_image`).
- Claim: an image whose box animates (its own width or height, or an
  ancestor's) asks for a new decode at each 1.25x step as it grows, and
  for cover, at each aspect change past 2 device px. At most one decode
  per image is in flight, so it costs a busy worker, not a backlog.
- Why deferred: skipping requests while a node transitions misses
  ancestors' transitions, and a skip that ends without a final request
  leaves a stale bitmap; the steps already bound it.
- Resolves in: asking for the transition's end size up front (the
  animation knows it) when a consumer animates images.

### DF-38: a new `src` of another shape decodes twice when unsized

- Source: PR #8 review (images) fix (own finding).
- Where: crates/ui/src/image.rs (`image_result`).
- Claim: the old image, and its natural size, stay until the new one's
  pixels arrive (PR8-02). A node with no size of its own is laid out at
  the old natural size meanwhile, so the first decode of a new `src`
  with another aspect is planned for the old box; when it lands the
  node relayouts to the new natural size and decodes again. A sized box
  decodes once.
- Why deferred: taking the new natural size at the probe resizes the
  box around the old picture (a jump, then the pixels); the double
  decode is the cost of no jump.
- Resolves in: planning the decode for the new natural size when the
  node's size depends on it, if the double decode shows.

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
- PR5-01 (vectors review): a bad value in a drawing closed the session: a drawing whose strings or numbers do not parse now applies and draws nothing (`asset: None`, zero intrinsic size), as SVG draws nothing for an empty view box. Structural errors still reject (bad refs, a drawing on another kind, too many shapes, a malformed op, the byte cap). The facade coerces with `Number()` and drops a shape with a number that is not finite (warned once), clamps `opacity`, gives a negative `strokeWidth` or a miter limit under 1 its default, draws no shape for a size of zero or less, and takes `strokeDasharray` as a number, an array or a string (a negative length draws solid).
- PR5-02 (vectors review): the bounds were per string: they are per drawing now. `check` caps the shapes' string references at 4 MiB (`MAX_BYTES`) before the key is built; `build` parses each distinct string once, charges path commands per shape, transform functions and points to one `MAX_VERBS` budget, and shares one dash budget. The key is built once per transaction and shared as `Arc<[u8]>` by the executor, the source table and every node drawing it.
- PR5-03 (vectors review): zero-length dashes at a subpath's start and end are drawn (dots at 0, 4, 8 and 12 for "0 4" on a 12-unit line, as Chrome), and a closed subpath that starts and ends inside a dash emits its last piece first, so the corner gets a join.
- PR5-04 (vectors review): `Vector`'s `opacity` is the node's (`style.opacity`: one layer, animatable); only `G` and shape opacity multiply into shapes. DF-16 is corrected: what remains is `G` opacity and fill under stroke.
- PR5-05 (vectors review): tessellation is bounded by what shows: items whose bounds miss the viewport are skipped, the tolerance is at least the item's size over 2^16 (a circle of radius 4e6 through a 24-unit icon flattens to under 2,000 vertices instead of over 5,000, and the count no longer grows with the radius), and a shape at opacity 0 is not tessellated. A node at opacity 0 still is: its opacity is a layer patch, so a fade-in does not tessellate on its first visible frame.
- PR5-06 (vectors review): `fillOpacity` and `strokeOpacity` scale the color's alpha; `currentColor` paints with a `color` prop on `Vector` or `G` and throws without one. DF-14 is narrowed to "no inherited color".
- PR5-07 (vectors review): `flattenShapes` throws past 4,096 shapes, or 4 MiB of strings, saying why, instead of a wrapped count or a closed session.
- PR5-08 (vectors review): the docs say what the bounds are, list the kit's remaining differences (token colors, hook-using wrappers; DF-14, DF-15), and label the benchmark as the Rust direct API. The host node keeps the drawing it last sent, so a resend stringifies once, not twice; the JS side of a resend is measured (EXPERIMENTS.md).
- PR5-09 (vectors review): the test gaps are filled: dots, offsets past a period, a closed subpath starting inside a dash; packed arc flags, skews, negative view boxes, T and S after other commands; sweeps shrinking both tables after churn, scale changes, a source removed and reused in one transaction, id recycling while shared; unknown kind, rule, join and cap bytes and a bad string ref; Ellipse and Polyline, asset and shapes switching, a view-box-only resend, an inherited dash array, and the numeric-string and NaN cases.
- PR5-10 (vectors review, nit): a payload must start with the `CRV1` magic, checked before the source table (drawing keys start `CRVS`), so a payload cannot take a drawing's asset. The rebuild oracle relied on that hole (it replayed a drawing's key as a payload); it replays the drawing now (`Drawing::from_key`), and its random sequences create drawings as well as assets.
- PR5-11 (vectors review, nit): a Vector going from shapes or an asset to neither sends an empty drawing, which draws nothing.
- PR5-12 (vectors review, nit): opacity 0 does not tessellate (PR5-05).
- PR5-13 (vectors review, nit): `VectorProps` is a union: an asset Vector's type has no shape props, and `asset` is dropped when `viewBox` is given.
- PR6-01 (zorder review): an owner layer closed with its last child while a layer it owned stayed open, which then lost its owner for good: a container stays open while it has children or open layers it owns, closing cascades to an idle owner, and `Layer`'s cleanup effect is gone (tested with the review's repro, which fails on the old code, and the cascade).
- PR6-02 (zorder review): the harness snapshot sent `LAYER` before the owner's create when the owner's id was higher: layer ops go in a pass after every create, and `Gen` sets layers with random owners (any node, none, later removed).
- PR6-03 (zorder review): a style of only spatial keys sent a layout op (`{}` against no style): `layoutPart` returns undefined when no defined key is left (tested undefined, `{zIndex: 1}`, undefined).
- PR6-04 (zorder review): the randomized order test compared the index against a walk reading the same order: a new oracle over random z, layers, owners, moves, detaches and reused ids checks the stable sort by z, each layer above its owner's sibling, the drawn order and hits, before and after the refresh (dropping the owner raise or the stale-reader re-sort fails it); new tests cover a reused sorted parent's id, z and `LAYER` set while detached, an app root remounted under an open layer, an insert before a sibling in a layer, and Suspense hiding and revealing a layer's children.
- PR6-05 (zorder review): the scene walk copied each sorted parent's order per visit: it lends the order out of `Host::orders` for the walk (`mem::take`) and puts it back.
- PR6-06 (zorder review): a non-integer `zIndex` threw in the commit: it is rounded and clamped to an i32, NaN is 0, with a warning logged once.
- PR6-07 (zorder review): closing a layer missing from the open list would have dropped the last one: the splice is guarded.
- PR6-08 (zorder review): `LAYER` was accepted on any kind: it is a structural error on anything but a View (tested); `order.rs` and `Mutation::Layer` document the owners that silently count as none.
- PR6-09 (zorder review): between a transaction and a refresh, a direct `hit_test` sorts stale parents on the spot: documented on `hit_test` (it takes `&self`, so it cannot refresh lazily).
- PR6-10 (zorder review): EXPERIMENTS says the frame column excludes the re-sort; DF-19 adds that an app root with a positive `zIndex` covers unowned layers; DF-20 is closed.
- DF-20 (an owner-only layer closed when Suspense hid it): fixed in the PR #6 review (PR6-01); a Suspense hide removes no layer's children, so nothing closes.
- PR7-01 (states review): transitions fired on mount and on the first frame (a row mounted selected faded in; a narrow window at launch tweened its heights): a table's first resolution and the first `render`'s environment write straight to the rows and cancel an animation on the same property (`set_window_size` gives the drivers the size before the first frame); tested with a fill transition at mount and at the first narrow frame.
- PR7-02 (states review): layout values were paired with the element's base per wire field, so `_narrow: { padding: { left: 4, right: 4 } }` carried the base's top and bottom over `_compact`'s: layout travels as a u64 of keys, one per property, axis and side, then the fields that hold them, and native composes per key; the facade's pairing is gone. Padding 16/12 with `_narrow` 4 and `_compact` 6 on the sides gives 4/6 at compact width, and `_narrow: { height }` with `_hover: { width }` gives both (tested natively and in bun). Border color and width are separate values too (`BORDER_WIDTH`, bit 7).
- PR7-03 (states review): a variant's `display` overrode Suspense's hide: the facade strips `display` from the variants of a hidden or suspended node and sends the table again on reveal (bun test).
- PR7-04 (states review): detaching or removing a node kept a press inside it: `Detach` and `Remove` end it (no click on release); focus stays on a node that moves, documented, and its scopes' bits follow when it is placed again (tested).
- PR7-05 (states review): an accessibility Focus or Blur did not restyle: `a11y_action` restyles, so `_focusWithin` follows (tested).
- PR7-06 (states review): bare modifiers switched to keyboard modality: Shift, Cmd and the like alone, and Cmd, Ctrl or Alt chords, leave focus-visible as it was (tested).
- PR7-07 (states review): leaving the window left the pointer at (-1, -1): a new `PointerLeave` event clears the position and ends hover, and a wheel event sets the position (tested).
- PR7-08 (states review): hover at rest hit-tested every animating frame: it skips when no table and no listener reads hover (counted per table and in the host's listener count; tested with a reader appearing later).
- PR7-09 (states review): input bits were recomputed on every event: they return early when the hovered node, the press, the focus, the modality and the structure revision are unchanged.
- PR7-10 (states review): a color tween walked the subtree each frame: the inheriting spans under a `COLOR` node are cached per node, keyed by the structure and text revisions and a count of `COLOR` nodes set or cleared.
- PR7-11 (states review): VARIANTS was unbounded: at most 256 variants on a node and 8 terms in a variant, rejected by validation (tested).
- PR7-12 (states review): unsupported variant keys were dropped silently: every key a variant does not apply is logged once, naming the key and DF-29 (bun test); DF-23's wording now matches, and DF-29 lists the kit gaps.
- PR7-13 (states review): a disabled Pressable was half disabled: it is not focusable, reads as disabled to assistive technology, and native masks pressed and focus-visible under `disabled` as it did hover (tested natively and in bun).
- PR7-14 (states review): toggling `group` remounted the children: the scope Provider is always rendered (bun test).
- PR7-15 (states review): specificity compared every rank: it is depth, then the latest rank tested, then declaration order, as the spec says. `_selected._pressed` against `_hover._pressed`, both on, tie at pressed, so the later declared wins where the full compare picked selected (tested).
- PR7-16 (states review, nit): `_hover` on a non-scope element differs from Marbre web: recorded in DF-29 and topic 5.
- PR7-17 (states review, nit): inherited color follows the native tree and scopes React's, so Portal content under a colored Pressable draws its own color: documented in topic 5 and tested in bun.
- PR7-18 (states review, nit): DF-28 says what cancels an animation on a tabled node, per key (tested).
- PR7-19 (states review): the harness's one allocation per hover was its own event vector: events drain into a kept buffer (`Ui::drain_events`), and a "hover, no scopes" row gives the reference (0 allocations; 116 µs for 1,000 dependents against 4.2 µs for none).
- PR7-20 (states review): the breakpoint delta was not restyle alone: a row that sends the 1,000 heights directly with no tables splits it (about 200 µs of layout the heights cause anyway, 130 µs of restyle).
- PR7-21 (states review): ARCHITECTURE §13 said restyle recomputes every scope's input bits: it says the three ancestor chains, and when.
- PR7-22 (states review): the untested items have tests: StrictMode and Suspense with scopes, a group toggle, a Text with no color above (white), `COLOR` and scopes through a Portal, table limits, `_touch` with hover masking, and a Text moved under another `COLOR`.
- PR8-01 (images review): the decoder did not bound memory (a 249 KB PNG took 1.25 GB): the probe and the decode reject over 64 megapixels (`MAX_PIXELS`) before any buffer, the codecs check 32,768 px a side, and `decode` reserves the decoded buffer, plus the RGBA copy for a format that needs one, against 512 MiB (`MAX_ALLOC`) before `from_decoder`. The rotation copy is gone: the decoder crops and shrinks in the stored orientation and turns the small result. A probe over the budget fails the image, so no huge intrinsic size is set. The false "512 MiB" comment is replaced. Tests: a 4 x 4 PNG claiming 20,000 x 20,000 fails in both, and the reserve fails one byte short for RGBA and gray.
- PR8-02 (images review): a new `src` blanked the image and collapsed its layout until the decode landed: the old bitmap and natural size stay until the new image's first pixels or failure. An unsized image with a new aspect decodes twice as a result (DF-38).
- PR8-03 (images review): a failed fetch left the previous `src`'s image on screen: the facade sends empty bytes, which clear it, and fires `onError`.
- PR8-04 (images review): the worker queue had no cancellation or priority: a shared queue replaces the channel. Probes run before decodes, a newer decode of an image replaces its queued one, and the queued work of a removed or replaced image is dropped (`Ui::take_dropped_images`). A decode already running finishes (DF-33).
- PR8-05 (images review): images drew dark edges when magnified and dark rims where transparent, with no shader change: the decoder averages premultiplied and stores straight alpha, transparent pixels take their visible neighbours' color, and a color raster's atlas gutter repeats its edge pixels. The llvmpipe test checks edge rows and columns of each fit, and a transparent-black ring stretched 4x shows no dark pixel; both fail without the fix.
- PR8-06 (images review): resizing thrashed full decodes: a grow asks for 1.25 times the needed size (capped by the source and a page), and a cover crop within 2 device px of the plan keeps its bitmap. Skipping requests during layout transitions is not done (DF-37); DF-34's "a frame of blur" is corrected to a decode's time.
- PR8-07 (images review): image memory had no budget: the core's pixel copies are capped at 64 MB, least recently drawn dropped first; an evicted raster whose copy is gone decodes again. Large images out of the atlas and page reclaim are DF-36.
- PR8-08 (images review): a panic in the worker hung every image for good: `run` is caught and its image fails ("the image decoder panicked"); a worker that dies anyway (a panic the catch cannot hold) fails its image ("the image decoder stopped") and is replaced at the next pump. There is no send to fail, so no node stays pending.
- PR8-09 (images review): a decode failure kept the probed size, and a later failure hid a loaded image: a failure with no bitmap clears the natural size; a failure after a load keeps the bitmap and is not retried.
- PR8-10 (images review): decode failed on an unreadable EXIF chunk where probe ignored it: both show the image unturned (`unwrap_or(NoTransforms)`). No test: PNG's EXIF read cannot fail, and the other codecs need a crafted file.
- PR8-11 (images review): equal bytes returned early with no event: they report again (`LOADED` if loaded, else the stored failure; a load in flight reports when it lands). The `src` doc says a new `Uint8Array` each render is sent each time.
- PR8-12 (images review): `alt=""` made an unlabeled image: it now has no role and no label, which assistive technology skips.
- PR8-13 (images review): the drawn rect was not snapped: its origin rounds to device pixels, the size stays the fitted size.
- PR8-14 (images review): fetch handling was thin: an unmount or a new `src` aborts the fetch (no `onError`), a load has 30 s before it fails with `onError`, and the doc says a relative path resolves against the process's working directory. Tests: a fetch failure after a change clears the image, an unmount aborts, and A, B, A with URLs sends A again.
- PR8-15 (images review): docs and numbers: the bench builds the probe request outside the timed part (the PNG probe was the 23 MB copy) and adds RGBA, gray and 16-bit PNG rows (EXPERIMENTS.md); DF-34 is corrected, no color management is DF-35, and DF-32 lists ICO favicons.
- PR8-16 (images review, nit): a quad over 65,535 device px drew at the wrong size: the quad and the drawn rect shrink together, keeping the aspect. `capture.rs` is left as is.
- PR8-17 (images review, tests missing): added: EXIF, all eight orientations against the `image` crate's (size and pixels, at 1:1 and shrunk); the bomb and a truncated file after a good probe; a new `src` keeping the old image; a fetch failure after a change, an unmount during a fetch, A, B, A; eviction and re-insert, and a copy dropped past the budget decoding again; a scale change; a cover aspect change and late pixels for an older crop; edge and transparency pixels on llvmpipe; a panicking codec and a dead worker.
