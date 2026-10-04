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

### DF-17: accessibility reads layers after the app

- Source: sibling z and layers (work item 4) implementation (own
  finding).
- Where: crates/ui/src/a11y.rs.
- Claim: a layer container is a root-level node after the app's roots,
  and the accessibility tree keeps tree order. So a screen reader reads
  a menu opened from a toolbar button after the whole app, in open
  order, not next to the button.
- Partly resolved (work item 3, focus traps): Tab follows owners. An
  owned layer's focusables come right after its owner's subtree
  (`trap.rs`, `tab_order`), traps and `modal` include the layers they
  own, and a modal leaves only its scope in the accessibility tree.
- Why deferred: reading a layer next to its owner is the accessibility
  pass's (topic 13): AccessKit children would have to leave tree order,
  or the owner point at the layer (`aria-owns`, `controls`).
- Resolves in: topic 13.

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
- Since work item 6 (keyframe animations), everything downstream of
  the bit is there. `Ui::set_reduced_motion` sends an ENVIRONMENT event
  (out kind 21), and the facade re-sends every transition and animation
  under each entry's `reducedMotion` policy. Only the platform source
  is missing.
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

- Source: work item 5; the PR #7 review (PR7-12).
- Where: packages/bridge/src/host.ts (`variantValues`).
- Claim: Marbre's variants also set `z`, `pointerEvents`,
  `visibility`, elevation and the focus ring; Craie's apply paint,
  opacity, transform parts (percent translates included, since work
  item 6) and layout only.
  `_hover={{ pointerEvents: "none" }}` or `style: { zIndex: 2 }` in a
  variant is logged once and left out, as is every other key a variant
  does not apply.
- Why deferred: each needs its own native value in the table (z
  re-sorts the parent, pointer events and visibility change hit
  testing, the focus ring and elevation are paint sources), and none
  is on a screen the kit ports first.
- Resolves in: when a ported component needs one.
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

### DF-39: imported SVGs bake `currentColor`

- Source: PR #12 review (inherited color), R12-03.
- Where: tools/svg-import/src/lib.rs (`paint`: usvg resolves
  `currentColor` before the tree reaches us).
- Claim: usvg resolves `currentColor` at import against the SVG's own
  `color`, black by default. An icon imported as CRV1 with
  `stroke="currentColor"` draws black on a dark UI and ignores
  `_hover={{ color }}`, with no warning. The same icon sent as runtime
  shapes (`<Path>` and the other elements) inherits the node's `COLOR`.
- Why deferred: out of PR #12's scope (runtime drawings); the kit's
  icons are runtime shapes.
- Resolves in: a pre-scan for `currentColor` (as the one for dropped
  elements) that emits `Paint::Current` for the affected paints, or at
  least a report note at import.

### DF-40: `expanded` is silent on macOS and Linux

- Source: PR #14 review (a11y states), M1.
- Where: crates/ui/src/a11y.rs (`set_expanded`); upstream
  accesskit_macos 0.27 and accesskit_atspi_common 0.20.
- Claim: Craie puts `expanded` in the AccessKit tree where the prop was
  given, but only the Windows (UIA ExpandCollapse) and iOS adapters read
  `is_expanded()`. On the Mac, `<Pressable expanded={open}>` is "button"
  to VoiceOver, never "collapsed" or "expanded"; Orca on Linux hears
  the same.
- Why deferred: the gap is upstream; Craie's tree is right and serves
  Windows today.
- Resolves in: AccessKit support (macOS `AXExpanded`, AT-SPI
  `State::Expandable` and `State::Expanded`), upstreamed or patched, and
  an AccessKit bump.

### DF-41: accessibility states and roles the kit uses that Craie lacks

- Source: PR #14 review (a11y states), m5 and n1.
- Where: crates/ui/src/mutation.rs (`Role`), crates/ui/src/a11y.rs,
  packages/bridge/src/wire.ts (`ROLE`).
- Claim: `pressed` (toggle buttons), `mixed` (indeterminate checkboxes)
  and `highlighted` don't reach assistive technology, and the menu roles
  (`menu`, `menuitem`, `menuitemcheckbox`, `menuitemradio`) don't exist.
  The kit's select (`select.native.tsx`) builds `menuitemradio` with
  `aria-checked`, so on Craie an adapter falls back to `button` and the
  item's checked state is styling only.
- Why deferred: out of PR #14's scope (check roles and three states);
  `CheckInputProps` has no indeterminate.
- Resolves in: the accessibility pass (ARCHITECTURE-update §13): the
  roles append after `radiogroup` (a protocol bump), check menu items
  report toggled like the check roles, `pressed` on a button is toggled
  (AccessKit's toggle button), and `mixed` needs a value past one bit.

### DF-42: no `onLongPress` or `onMiddlePress`

- Source: work item 3 (presses and activation).
- Where: crates/ui/src/press.rs, packages/bridge/src/index.ts
  (`PressProps`).
- Claim: the kit's `PressableProps` has `onLongPress` (a held press) and
  `onMiddlePress` (the middle button), and Craie's Pressable has neither.
  A long press is a click on release, and a middle press presses nothing,
  so "open in a new tab" on a middle click of a row does nothing.
- Why deferred: the brief allowed it. A long press needs a native timer
  that cancels the click, with a threshold per platform. A middle press is
  a second `ACTIVATE` source (button 2) that the kit's web version maps to
  `auxclick`.
- Resolves in: a `PRESS` phase for the long press after a hold (the
  release then sends no `ACTIVATE`), and middle-button presses reported
  with button 2, which the facade routes to `onMiddlePress`.

### DF-43: pressable spans are pointer-only

- Source: work item 3 (presses and activation).
- Where: crates/ui/src/press.rs (`span_pressable`), crates/ui/src/a11y.rs.
- Claim: a nested `<Text onPress>` (a link in a sentence) activates on a
  click only. It's no Tab stop, Enter can't reach it, and assistive
  technology sees one text node with no link inside, so VoiceOver can't
  click it (the paragraph's own click goes to the pressable around it).
  A web `<a href>` in a paragraph is all three.
- Why deferred: a span has no node of its own to focus or to put in the
  accessibility tree. Both need per-span nodes (DF-1's cluster mapping)
  and topic 11's interactive spans.
- Resolves in: topic 11: a span-level focus target and a link node per
  pressable span in the accessibility tree, activated through
  `Ui::activate` with the span.

### DF-44: a keep-focus press clears focus-visible

- Source: work item 3 (presses and activation).
- Where: crates/ui/src/dispatch.rs (`pointer_down`, modality).
- Claim: Tab to the composer's send button, then click the mention button
  (`preventFocusOnPress`): focus stays on send, but its ring goes away,
  since any pointer press leaves keyboard mode. Chrome keeps the ring
  when a click moves no focus.
- Why deferred: the brief's rule ("a pointer press turns it off") is the
  one built.
- Resolves in: keeping keyboard mode across a press that moves no focus,
  if the kit wants Chrome's behavior.

### DF-45: a release on another node clicks nothing, where the web clicks the common ancestor

- Source: work item 3 (presses and activation), review of #17.
- Where: crates/ui/src/press.rs (`press_up`).
- Claim: press Archive, drag onto the row body and release. The kit on
  the web uses native `onClick`, and the browser fires `click` on the
  nearest common ancestor of the pointerdown and pointerup targets: the
  row opens. Here the release is outside Archive, so Archive gets `PRESS`
  out and nothing activates. Pressing the row and releasing on Archive
  activates the row in both.
- Why deferred: the rule built is React Aria's (release on the pressed
  node or inside it), which the review and the lead judged the better UX:
  a press abandoned by dragging off stays abandoned.
- Resolves in: nothing, unless the kit needs parity with its web build;
  then `press_up` would activate the innermost pressable around both the
  press and the release targets.

### DF-46: a paragraph update cancels a press on its spans

- Source: work item 3 (presses and activation), review of #17 (PR17-06).
- Where: crates/ui/src/press.rs (the span press's revision check).
- Claim: a press on a pressable span remembers the paragraph revision it
  started on, and any new paragraph op for that Text cancels it. So in
  `<Text>Updated {ago} · <Text onPress={retry}>Retry</Text></Text>`, where
  `ago` ticks every second, a slow click on Retry can land after a tick
  and activate nothing.
- Why deferred: native doesn't know which span belongs to which pressable
  Text, so it can't tell a text change from an owner change; cancelling
  is the safe side. Live text next to a link is rare in the kit.
- Resolves in: when a screen puts a link in ticking text; the paragraph
  op would then carry span owners, and only an owner change would cancel.

### DF-47: a `FocusTrap` is a layout box

- Source: work item 3 (focus traps) implementation (own finding).
- Where: packages/bridge/src/index.ts (`FocusTrap`).
- Claim: the trap is a View, so it takes part in layout: in a row, its
  children lay out in the trap's box, not in the row. The web kit's
  trap adds no box.
- Why deferred: Craie's layout has no `display: contents`. The trap
  takes a `style` meanwhile.
- Resolves in: `display: contents` in the owned layout engine, or the
  trap op on the first child (one child only).

### DF-48: `autoFocus` does not focus on mount, outside a trap or inside one holding the focus

- Source: work item 3 (focus traps) implementation (own finding),
  widened by review of #18 (PR18-06).
- Where: crates/ui/src/trap.rs, packages/bridge/src/index.ts
  (`autoFocus`).
- Claim: `autoFocus` marks what a trap focuses when it activates, and
  a node mounting into an active trap takes the focus only when the
  focus is outside that trap. A `TextInput autoFocus` in a page with
  no trap is not focused when it mounts, as React Native's would be;
  nor is wizard step 2's field while focus sits on the dialog's Back
  button, where React's `autoFocus` would move it.
- Why deferred: out of the traps' scope; the kit calls `focus()` from
  an effect meanwhile.
- Resolves in: focus on mount for any `AUTO_FOCUS` node (the settle
  pass already sees each one mount), if the kit needs it.

### DF-49: no percentages in an RN transform list

- Source: work item 6 (transform parts).
- Where: packages/bridge/src/wire.ts (`transformMatrix`).
- Claim: React Native accepts `{ translateX: "50%" }` in a transform
  list; Craie's list takes points only, because it folds into the free
  matrix, which has no size-relative part. A percentage (or any
  non-number) in a list's translate or scale throws "percentages go in
  style.translate (DF-49)". `style.translate: ["50%", 0]` does the same
  and follows the size.
- Why deferred: the wire's fractions add straight to the translation,
  so a list's percent step maps onto them only when no rotate, scale or
  skew comes before it in the list; anything else needs a size-relative
  term per matrix column.
- Marbre does write them: its native resolver emits the `roll-up`,
  `roll-down`, `roll-left` and `roll-right` enter presets as
  `translateY`/`translateX` "±100%" list steps
  (packages/ui/src/style/resolve.native.ts:186-189), and turns the
  kit's `translateX`, `translateY`, `scale` and `rotate` style keys
  into a list (:317-321; `translateX: '50%'` included). So the Craie
  kit adapter must map those keys and presets to parts (`translateX`,
  `translateY`, `scale`, `rotate`), not to `transform`. The list's
  order there is translate, scale, rotate; the kit's scale is one
  number, which commutes with rotate, so parts' translate, rotate,
  scale draws the same.
- Resolves in: the kit adapter mapping (above); the list itself only
  if a ported component writes a percent step by hand.

### DF-50: arrows don't flip right to left

- Source: work item 3 (focus groups) implementation (own finding).
- Where: crates/ui/src/group.rs (`group_key`).
- Claim: → always moves to the next member in tree order. In a
  right-to-left layout the next member is drawn to the left, so a
  horizontal toolbar in Arabic moves against the arrow. React Aria and
  the WAI-ARIA practices flip ←→ there.
- Why deferred: Craie has no layout direction yet.
- Resolves in: a direction in layout; `group_key` then swaps ←→ for a
  group laid out right to left.

### DF-51: `both` moves in one line, not a grid

- Source: work item 3 (focus groups) implementation (own finding).
- Where: crates/ui/src/group.rs (`group_key`).
- Claim: a `FocusGroup orientation="both"` over a 3 x 3 grid of swatches
  moves → and ↓ alike to the next swatch in tree order, so ↓ from the
  first goes to the second, not to the one below it.
- Why deferred: a grid needs rows (a two-axis group, or geometry), and
  the kit's groups are lines.
- Resolves in: a grid group, with row structure from the tree or from
  the members' boxes, if the kit builds grids.

### DF-55: no dash offset channel in keyframes

- Source: work item 6 (keyframe animations).
- Where: crates/ui/src/keyframes.rs (`Sample`), packages/bridge/src/motion.ts
  (`frameValues`).
- Claim: the target lists a vector shape's stroke dash offset among the
  frame values. A frame carries opacity, the transform parts and the
  three paint colors only, so a marching-ants border or a drawn-on
  stroke can't loop natively.
- Why deferred: the dash offset lives in the shape's paint (vector
  ops), not a row the keyframe driver writes. It needs a channel and
  an `absorb` path of its own.
- Resolves in: when a ported component animates a stroke; with blur
  and shimmer frames in work item 7 if convenient.

### DF-56: variant animation ends are not reported

- Source: work item 6 (keyframe animations).
- Where: crates/ui/src/states.rs (`variant_animations`),
  packages/bridge/src/host.ts (`dispatchEvent`).
- Claim: `onAnimationEnd` fires for `enter` and `animation` entries
  only. A finite animation under `_pressed: { animation }` finishes
  unreported, because native starts and stops variant animations
  without JS and sends them with notify off.
- Why deferred: nothing waits on one. Kit variant motion is loops and
  presses. The key already carries the variant's block number
  (review #21), so reporting needs only notify and a JS lookup.
- Resolves in: the first consumer that chains work on a state's
  animation.

### DF-58: an `enter` with a forwards fill holds for good

- Source: work item 6 (keyframe animations), review #21 m5.
- Where: crates/ui/src/keyframes.rs (`run_keyframes`,
  `declare_animations`), packages/bridge/src/index.ts (`enter`).
- Claim: an `enter` with `fill: "forwards"` or `"both"` holds its last
  frame over the node's own value after it ends, and nothing clears
  it: an enter op applies only in the transaction that creates the
  node, so no later op can re-declare or drop it. Take `enter={{
  keyframes: [{ at: 1, opacity: 0.5 }], fill: "forwards" }}`: a later
  `style={{ opacity: 1 }}` doesn't show. The default fill
  (`backwards`) and `none` let go at the end, as intended.
- Why deferred: it is what the declaration says (CSS holds a forwards
  fill as long as the animation is applied, and an enter is applied
  for the node's life), and no kit preset uses a forwards enter. An
  empty `enter` op after creation could clear it if one ever needs to.
- Resolves in: exits (next PR), if an exit needs to take over from a
  holding enter; else the first consumer that hits it.

### DF-59: a running `enter` keeps on when reduced motion turns on

- Source: work item 6 (keyframe animations), review #21 docs nit.
- Where: packages/bridge/src/host.ts (`setReducedMotion`).
- Claim: turning reduced motion on re-sends each node's transitions,
  `animation` list and variants under the new policies, but not
  `enter`: an enter applies only at creation, so a re-send would do
  nothing. An enter already running when the setting flips plays out
  at full length (at most its duration, 200 ms for the kit's).
- Why deferred: the window is one enter's length, and cutting it
  short needs an "end now" op for a running enter.
- Resolves in: with exits (next PR), which need to end enters early
  too, or when the platform bit lands (DF-25).

### DF-60: a reversed transition runs its full duration

- Source: work item 6 (keyframe animations), review #21 m1.
- Where: crates/ui/src/animation.rs (`Ui::transition`).
- Claim: CSS shortens a transition that reverses mid-flight (its
  "reversing shortening factor": leaving hover 30 ms into a 120 ms
  scale-in takes about 30 ms back). Craie retargets from the current
  value with the full duration of the style being entered, so the
  same exit takes the base's whole timing.
- Why deferred: not small. It needs the reversing-adjusted start value
  and factor kept per tween (CSS Transitions §3.4.4), for a difference
  that shows only on quick in-and-out gestures.
- Resolves in: with DF-5 (retargeting restarts the full duration),
  which touches the same path.

### DF-61: List rows don't exit

- Source: work item 6 (exits).
- Where: crates/ui/src/exit.rs (`detach_node`).
- Claim: a List's row removed with an `exit` goes at once, and native
  answers `skipped`. A chat list whose messages fade out when deleted
  just drops them.
- Why deferred: a List windows its rows itself (topic 12). An exiting
  row would need a slot in the list's measure and anchor, which the
  list rework (work item 11) redesigns anyway.
- Resolves in: work item 11, if a ported list animates removals.

### DF-62: a collapse stops at padding, border, min size and gap

- Source: work item 6 (exits).
- Where: crates/ui/src/keyframes.rs (size channels), layout.
- Claim: a `height: 0` frame sets the border-box height, and layout
  floors it at the padding and border (and a `minHeight`). A toast with
  `padding: 12` collapses to 24, then vanishes at the end, and its
  parent's `gap` stays until then. The app puts the padding on an inner
  View to collapse fully.
- Why deferred: collapsing padding and gap too means tweening more
  layout values per exit, for a jump of a few points at the end.
- Resolves in: when a ported component shows the jump; tween the
  padding (and the gap share) along with the size.

### DF-63: size frames are exit-only, in points

- Source: work item 6 (exits).
- Where: crates/ui/src/executor.rs (validation), packages/bridge/src/motion.ts
  (`frameValues`).
- Claim: `width` and `height` frames exist in `exit` only, and only as
  points. An `enter` that grows a row from 0 to its natural height, or
  a frame at `50%`, is rejected. The implicit end of an exit is the
  laid-out size, which covers the collapse.
- Why deferred: growing from 0 needs the natural size (`auto`) as an
  endpoint, a measure before the frame, which layout transitions (out
  of scope) will bring.
- Resolves in: layout transitions.

## Closed

- DF-57 (work item 6, review #21 M1): end indices were wire indices
  mapped back through the last list sent, so an old list's ends could
  read the new map. An animation's identity is now its index in the
  author's list, sent on the wire: the end event carries it and the
  host's `motionIndex` map is gone.

- DF-22 (work item 6, keyframe animations): a variant could change
  what a property is, not how it moves. A variant's `style.transition`
  now replaces the node's transition list while the variant is the
  most specific active one with a list, as CSS does (review #21 m1;
  Marbre's web kit emits a whole list per variant), and its
  `animation` runs natively while it holds: `_hover: { style: { scale:
  1.02, transition: { scale: { duration: 120 } } } }` scales in over
  120 ms and back with the base's timing, and an opacity change during
  that hover jumps. `transition: "none"` in a variant times nothing.
  CSS's shortening of a reversed transition is DF-60.

- DF-21 (work item 6, transform parts): a variant's transform replaced
  the whole matrix. The spatial row now holds translate, rotate, scale
  and the free matrix as parts, and a variant sets only the parts it
  names, per axis: `_hover: { style: { scale: 1.02 } }` keeps the base
  `rotate`.

- DF-19 (work item 3, focus traps): layers owned coarsely. A `Layer`
  opened inside a `FocusTrap` is owned by the trap's node, found through
  React context, else by the enclosing layer's container; a trap's
  scope, `modal` exemption and Tab position follow owners. Still true
  and accepted: a top-level `Layer` has no owner, so one with a
  negative z sorts under the app, and one at z 0 goes under an app root
  with a positive `zIndex` (the kit's layer tokens are all positive).

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
- PR7-16 (states review, nit): `_hover` on a non-scope element differs from Marbre web: recorded in DF-29 and topic 5. Wrong: Marbre web also reads the nearest scope; corrected in topic 5 by PR #11.
- PR7-17 (states review, nit): inherited color follows the native tree and scopes React's, so Portal content under a colored Pressable draws its own color: documented in topic 5 and tested in bun. Reversed by PR #11: a Portal or Layer starts a new scope chain too.
- PR7-18 (states review, nit): DF-28 says what cancels an animation on a tabled node, per key (tested).
- PR7-19 (states review): the harness's one allocation per hover was its own event vector: events drain into a kept buffer (`Ui::drain_events`), and a "hover, no scopes" row gives the reference (0 allocations; 116 µs for 1,000 dependents against 4.2 µs for none).
- PR7-20 (states review): the breakpoint delta was not restyle alone: a row that sends the 1,000 heights directly with no tables splits it (about 200 µs of layout the heights cause anyway, 130 µs of restyle).
- PR7-21 (states review): ARCHITECTURE §13 said restyle recomputes every scope's input bits: it says the three ancestor chains, and when.
- PR7-22 (states review): the untested items have tests: StrictMode and Suspense with scopes, a group toggle, a Text with no color above (white), `COLOR` and scopes through a Portal (reversed by PR #11: neither crosses), table limits, `_touch` with hover masking, and a Text moved under another `COLOR`.
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
- PR8-18 (re-review): a WebP's EXIF chunk size was allocated unchecked, past `Limits` (78 bytes asked for 4 GiB and aborted under a memory cap): before reading the orientation of a WebP, `webp_chunks_fit` walks its RIFF chunk headers, and one that claims more than the file holds skips the EXIF read (the image shows unturned). Test: the 1 x 1 file with a 0xFFFF_FFF0 EXIF chunk probes and decodes; without the guard the test binary aborts under `ulimit -v 3000000` ("memory allocation of 4294967280 bytes failed").
- PR8-19 (re-review): `Image` had no state styles after PR #7: `ImageProps` takes `Variants` and it renders through `useHost`, so `_pressed` inside a `Pressable` sends a VARIANTS table. Test added.
- PR8-20 (re-review): after a grow, an image stayed decoded 1.25 times too large, drawn minified: once the plan holds for a frame (the node looks again the next frame while its bitmap is off by more than a pixel), a bitmap of another size is decoded again at the drawn size (never upscaled, at most a page). The x1.25 step and the half-size hysteresis still apply while the box moves. Test: 80 px grown to 84 then 88 asks for 105, then 88, then nothing.
- PR8-21 (re-review): after URL A, bytes X, then URL B, A's bytes came back while B loaded: bytes reset the fetched state, so X stays up until B arrives. Test added.
- PR8-22 (re-review): a trimmed CPU copy could not always come back: `trim` and `ensure_resident` share one predicate (`State::restorable`: the bitmap is of the current payload, which has not failed), and only restorable copies are trimmed. An old `src` up while the new one loads, and pixels of a payload that later failed, keep theirs. Test: an old `src` evicted over the budget re-inserts from its copy.
- PR8-23 (re-review, nit): the snap was said to be node-relative only: it composes with the chunk origin, which snaps in world space (always in the window root, at rest in scroll and transformed spaces, as text does; §8). No DF: a test at 1.25x and 1.5x checks the quad's world origin (a node at x = 11 pt, 13.75 device px, draws from 14) and fails without the snap.
- PR8-24 (re-review, nit): `MAX_ALLOC`'s doc now says it bounds the image crate's buffers, and codecs may add about one more image. It stays 512 MiB.
- PR8-25 (re-review, nits): `take_dropped_images` says the embedder must call it or the list grows; the color gutter repeat applies to color emoji too (their opaque edges extend half a pixel instead of fading). Left as is: `Queue::add`'s per-image replacement, which one request in flight per image makes dead code for now, and the precision a low-alpha pixel loses at 1:1 through premultiply and back (invisible after blending).
- `craie-render --test paths` crashed now and then (a SIGSEGV in PR #5's first CI run, and perhaps PR #2's failure, whose log was not kept; characterised 2026-09-27): the crash is in the Khronos Vulkan validation layer (1.3.275, Ubuntu 24.04), which wgpu loads in debug builds. Every test created and dropped its own device; most likely, while one thread created or destroyed a device inside the layer, another thread's call on its own device jumped through freed layer state (one core catches a create and a destroy in flight; not matched to an upstream bug). All 13 cores had their top frame in `libVkLayer_khronos_validation.so`, across 9 tests and many wgpu calls. On exe1: 5 crashes in 200 runs with parallel tests; 0 in 200 with `--test-threads=1`, 0 in 200 with `WGPU_VALIDATION=0`; 8 in 200 with `LP_NUM_THREADS=1`, so llvmpipe's threads are not the cause. Fixed in the tests, with validation still on: `paths`, `gpu_uploads` (craie-harness) and `images` (craie-platform-winit) share one device per binary (`OnceLock`); `allocations` and platform-winit's lib tests take a lock instead, so their devices never overlap (`allocations` counts allocations per phase, and a shared queue could retire the other test's work inside one). After the fix, on exe1: `paths` 0 crashes in 200 runs, `gpu_uploads` 0 in 100, `images` 0 in 100. A new GPU test binary with several tests needs one or the other. On the Mac wgpu runs on Metal, which does not load this layer.
- PR9-01 (gpu-tests review): platform-winit's lib tests still opened a device each, in parallel (two in `app.rs`, and the `headless.rs` test's probe and run): all three take a crate-wide `GPU_TESTS` lock.
- PR9-02 (gpu-tests review): `allocations.rs` opened a device per test, in two tests: `GpuFrames` holds a lock until its device is gone. A lock and not a shared device, since a shared queue could retire the other test's work inside a measured phase.
- PR9-03 (gpu-tests review): the PR body's Verified section had placeholders: filled with the final run.
- PR9-04 (gpu-tests review): the mechanism was stated as fact: it is "most likely" in the LEDGER and the run log, which name the binaries fixed.
- PR9-05 (gpu-tests review, nit): the run log marked #9 merged while open: it merges with this text.
- PR9-06 (gpu-tests review, nit): one shared `Gpu` helper in craie-render instead of three local copies: not done. It would be public API for tests in a crate checked for wasm32, where `Gpu` is not `Sync`; the copies are four lines each and say why.
- PR10-01 (clippy review): the `field_reassign_with_default` allow's reason and the PR body said 19 sites, all in ui's tests: it fires at 32, across ui's tests, platform-winit, the harness and `examples/vector` (the deny-by-default `bad_bit_mask` error in craie-ui had stopped clippy before the crates that depend on it). The reason and counts are corrected; the allow stays workspace-wide.
- PR10-02 (clippy review, nit): `Values::valid`'s `bad_bit_mask` allow also covered the live `layout_keys` check: narrowed to one `let` holding the dead `value_field` check, which stays for when the mask widens.
- PR10-03 (clippy review, nit): `value_field::ALL` was the literal `0xFF`, so a retired flag would still count as known: it is the OR of the flags (0xFF today).
- PR10-04 (clippy review, nit): `lists.rs` sliced `reference[..n]`, whose length is `n` by construction: `reference.iter()`.
- PR10-05 (clippy review, nit): `bleed` allowed `manual_checked_ops` for its `n > 0` check: `NonZeroU32::new(n)` guards the three divisions instead, lint-free.
- PR11-01 (scopes review): the Layer cut was untested (undoing it kept every test green, since the Layer's Text read its inner Pressable either way): a bare `_expanded` directly in the Layer now logs "needs a scope" and sends no table.
- PR11-02 (scopes review): the bridge suite still flaked under load, in `layers.test.ts` ("reopens on top" failed 2 of 7 runs at load 35-50), which ticked once after a state update: a shared `settle` (tick until an op shows, at most 100 ticks, then once more) replaces every such single tick there and the text-root test's own helper.
- PR11-03 (scopes review): `disabled` was left out of `TextInput`'s types only, and a kit's props spread through still set DISABLED (an editable field read as disabled): `TextInput` drops it at runtime and logs it once (bun test).
- PR11-04 (scopes review): the smoke's Portal case read `_row` across the Portal, now dead (it logged "unknown variant key"): its View is a scope with `selected` and reads its own.
- PR11-05 (scopes review): a named group used across a layer logged only "unknown variant key": it says no group of that name is above, and that a Portal or Layer starts a new chain.
- PR11-06 (scopes review): the docs said a missing scope is a dev-time error in Marbre: it is a development log, on the unmerged `ui/state-scopes` branch, and Marbre's spec still says layer content keeps its opener's scope (topic 5 says so; the planning thread flips D28).
- PR11-07 (scopes review, nits): the TextInput JSDoc named `_focus` (not a key; `_focusVisible`); the text-root test's last step ticks once more past its predicate, so a later resend would show; the scope rules left DF-29 (kit values) for topic 5 only, and PR7-16, PR7-17 and PR7-22 say what #11 reversed; topic 5's facade bullet is reflowed; `ViewProps.group` no longer says toggling it remounts the children (PR7-14).
- DF-14 (no inherited color in drawings) and DF-24 (input color and vector `currentColor`): fixed in PR #12 (inherited color). A `currentColor` with no `color` on a `G` above no longer throws: the facade flags the shape's fill or stroke (a `current` byte in each DRAWING shape, protocol 5) and native paints it with the node's inherited `COLOR` (its own, else the nearest ancestor's, else white, as a span) times the shape's opacity. A `Vector`'s `color` is its node's `COLOR`, so its variants and transitions apply; a `G`'s stays in the drawing. A change patches one paint slot per shape: a hover recoloring 1,000 icons is a paint patch each, with no layout, chunk rebuild, tessellation or allocation (EXPERIMENTS.md, state styles). A TextInput's color is its `COLOR` the same way: INPUT_CONFIG no longer carries one, its variants apply, and the facade's warning is gone. The kit's token colors (`'ink-3'`) still resolve before they reach a prop.
- PR12-01 (color review): the runtime handshake still said protocol 4 (`craie_runtime_version`, `loadBindings`), so a stale `craie-node.node` passed the load check and failed at its first transaction: each side now answers with its own wire `VERSION`, and a bun test holds the cross-language fixture's header to the JS `VERSION` (Rust's `wire_fixture` decodes it only at the Rust one), so the two can't drift apart unnoticed.
- PR12-02 (color review): no test had more than one `currentColor` item, so a patch that stopped after the first slot survived: `current_color_resolves_nearest` now has a stroke, a solid, and a shape whose fill and stroke both inherit with different tints, and the harness drawing inherits on its square's fill (non-white tint) and its ring's stroke. With that mutant, the unit test and the incremental-vs-rebuild oracle fail.
- PR12-03 (color review): svg-import baking `currentColor` was untracked: DF-39.
- PR12-04 (color review, nit): a color patch walked every prepared item, and `inherits_color` every asset item per lookup: the cached meshes keep their `currentColor` tints beside them (`Meshes::tints`), so a patch walks only those, and a vector's `inherits` is found once per `set_vector`.
- PR12-05 (color review, nit): "text inherits" wording where inputs and drawings now inherit too: reworded at the listed sites and at four more (animation.rs, host.rs, states_tests.rs, ARCHITECTURE.md).
- PR12-06 (color review, nit): the GPU test covered `Scene::set_paint` only: `inherited_color_reaches_drawing_pixels` (harness gpu_uploads.rs) renders a Ui on a device: a `currentColor` square paints its parent's `COLOR`, and a new `COLOR` reaches the pixels with no chunk rebuilt.
- PR13-01 (run-log review): "the planning thread flips it" read as under way, while the planning thread is on the offline MacBook too: a "Waiting on the MacBook" part lists the allocation fix, the benchmark reruns, and Marbre's spec and native renderer cutting scopes at layers (#11's "What's left").
- PR13-02 (run-log review): "two tests deflaked, not loosened" was wrong: three fixes, and 5 to 30 s loosens a bound (justified: the test only checks the frame arrives); the 1.6 s is the whole test run alone.
- PR13-03 (run-log review): "all three are answered below" skipped gdb: each question is answered in one line.
- PR13-04 (run-log review): the overnight Decisions on protocol 4 and DF-14 say what #12 changed; Next steps says the two allocation tests fail on the Mac until the fix, and names them.
- PR13-05 (run-log review): 127 against 125 is two over: one allocation per pass that crosses 16 draws (+1 list, +2 four passes); "measured at 8 draws" is the Mac thread's reading. Wrong, as PR15-01 found: the +2 is per-pass work in wgpu, not command lists.
- PR13-06 (run-log review): an "Open risks" list: inputs inherit color silently, CRV1 kind 3 with no bump, clippy on macOS and wasm cfgs not in CI, the winit SIGSEGV at exit.
- PR13-07 (run-log review): DF-39 moved from the profile deferrals to "Known gaps" with TextInput `disabled`; EXPERIMENTS.md carries the pre-R12-04 caveat.
- PR13-08 (run-log review, nits): clippy's `bad_bit_mask` wording (checks that can't fire), the target-dir rule scoped to parallel agent runs, Erwin's go-ahead paraphrased from the message, jargon replaced (conflicts, reaching across a layer, color records, scene rebuilds), DF-29's reason spelled out, "Marbre web and native" narrowed to Marbre web (here and in topic 5), test counts named alike in the timeline, #12's macOS check placed.
- PR14-01 (a11y review): the PR and §13 said `expanded={false}` reads "collapsed", but the macOS and AT-SPI adapters never read `expanded` (Windows and iOS do): the code stays, the PR body, §13 and the `expanded` JSDoc say where it's heard, and DF-40 tracks the upstream gap.
- PR14-02 (a11y review): `selected` was reported on every role, and the kit styles checkboxes and radios with `selected` (a checkbox would read "checked, selected" on Linux): `a11y.rs` reports it on selectable roles only, Marbre web's `aria-selected` rule, which is the list row among Craie's roles. `selected_check_roles_report_checked_only` fails without the gate.
- PR14-03 (a11y review): a ROLE op that changed only the reported byte was untested (an executor comparing the role alone kept every test green): `reported_alone_updates` gives a button `expanded` and takes it back.
- PR14-04 (a11y review): the facade's scope gate on reported states was untested: the bun case has a scope-less `View` with `expanded` and expects no ROLE op; deleting the gate fails it.
- PR14-05 (a11y review): the PR said the widened dirty mask is why a toggle reaches the tree, but every transaction republishes it: the mask stays (it mirrors DISABLED), the docs and the tests' header say it's bookkeeping, and the tests assert the semantic revision for each of the four bits, so dropping any of them from `state_bit::A11Y` fails a test.
- PR14-06 (a11y review): the PR said a TextInput isn't a scope; it is (since #11): "What's left" is corrected, and the bun case checks a TextInput sends `[textInput, expanded | selected]`.
- PR14-07 (a11y review): the deferred states and roles weren't in this file: DF-41 (pressed, mixed, highlighted, the menu roles) and DF-40.
- PR14-08 (a11y review, nits): `radiogroup` is role 16 (still protocol 6; the fixture's layer container carries it), and ARCHITECTURE.md's §14 paragraph is rewrapped.
- MAC-01 (Mac verify): two `allocations` tests failed on Metal: `whole_frame_budgets` (opacity 0.5, four passes: 127 against 125) and `list_frames_do_not_allocate` (57 against 56): the wgpu budget was a flat per-pass constant measured at one draw count. On Metal (wgpu 30), a frame with one pass that draws costs 53 in wgpu besides the command lists; each opacity layer (its pass, the composite, the parent's resumed pass) costs 62 to 70 more (1 to 5 layers, as trackers grow); and wgpu-core records each pass into a fresh `Vec` that grows at 5, 9, 17, 33... commands (a pass is its draws plus 3 setup commands, plus 1 per rect/path pipeline switch). The budget is now 53 + 70 per layer + each pass's command-list growth, from a walk of the draw list that the test checks against `RenderStats` (passes, draws): exact for single-pass frames (56, 57), 127 against 130 for opacity 0.5. Craie's own phases still assert 0. Linux keeps the old constants until they're measured on exe1.
- MAC-02 (Mac verify): E19 phases that join native and JS stamps read 58,876,521 ms (js) and the same negative (apply): the probe read `CLOCK_UPTIME_RAW`, which stops during sleep, where Node's `process.hrtime` reads mach continuous time. It reads `CLOCK_MONOTONIC_RAW` now, and `report.py` refuses stamps outside dispatch → apply.
- MAC-03 (Mac verify): E19 round trips of 24 to 35 ms at p50 (125 ms at p95), idle, were the probe's own timers waking up to 150 ms late (macOS coalescing in an agent-started shell), counted as "wait". With the probe on, on macOS, the headless loop spins to every deadline and the windowed waker spins to each click: 1.01 and 0.29 ms at p50. Linux keeps its timed waits.
- PR15-01 (Mac review): MAC-01's model didn't explain 127 against 125: commands are draws + 3, so the steps come at 2, 6, 14, 30 and 62 draws, not 8 and 16, and the +2 isn't command lists at all. A backtrace diff of opacity 0.5 against 1 (+71) puts it in per-pass work that isn't uniform (attachment and bind-group bookkeeping, a Metal command buffer and encoder per pass) plus one-off texture barriers for the layer. The budget charges a fixed 53 per frame, 70 per layer, and each pass's own command-list growth, from a walk of the draw list checked against the existing `RenderStats` (no new renderer counters). Checked by mutation: one `Vec` per draw, per pass or per frame in the encode path each fails both tests.
- PR15-02 (Mac review): the probe's full spin also ran on Linux, and held the wake mutex while spinning: it spins on macOS only (the 20 ms `SPIN` elsewhere) and polls an `AtomicBool`, with the mutex left for the condvar; the windowed waker spins on macOS only too. E19 headless rerun on the Mac (load 3.0 to 3.6); the windowed rows are from c8ad34b.
- PR15-03 (Mac review): the clock comment called macOS's `CLOCK_MONOTONIC` the same clock as Node's: it's wall time since boot in µs; `CLOCK_MONOTONIC_RAW` is mach continuous time, which libuv has read since 1.44.
- PR15-04 (Mac review): "ruled out" cited QoS, a latency-critical activity and `mach_wait_until` with no numbers: it gives the late-wake medians (37, 55, 141 and 87 ms, each up to 150) and says the loops weren't kept.
- PR15-05 (Mac review): the exe1 E19 rows used the old probe; the lead adds the reviewer's exe1 rerun.
- PR15-06 (Mac review, nits): `report.py` fails on any negative skew, not just the median; the probe doc is rewrapped; the images range is 1.65 to 2.55 (JPEG 80 x 80 cover) and vectors 1.36 to 4.3.
- PR15-07 (lead, exe1): Linux gets its own wgpu constants, measured on Vulkan (llvmpipe, wgpu 30): 49 per frame and 54 per layer (50 for the first, 47 to 54 up to 5 layers). The command-list model fits there exactly (52 at 8 draws, 53 at 16 and 17, 106 for opacity 0.5). Under Metal's 53 and 70, one allocation per frame or per pass went uncaught on Linux; now each of the three mutations fails both tests there too.
- PR16-01 (run-log review): the run log blamed "the high E19 times Erwin saw" on the probe, but the late timers were reproduced only in the agent's shell: it says so, lists Erwin's own terminal as unmeasured, and gives the before and after (headless idle 23.85 / 124.67 → 1.01 / 1.19 ms).
- PR16-02 (run-log review): "the full suite passes on the Mac" had no commit: it ran at f1fb16e, before #14 (404 tests, not 411), the smoke test earlier at protocol 5 with the ad-hoc-signed addon, and the merged tree only on exe1. The run log and its open risk say so.
- PR16-03 (run-log review): Marbre's `CheckInput` "is" a scope overstated it: its Craie adapter is only a declaration. The run log says it will be, and adds a Marbre next step (the adapter passes `checked` and `accessibilityRole`). The review also said marbre#27 leaves protocol 6 marked Draft in `ui-kit.md`; Marbre main (db24bce) no longer does, so that part isn't carried.
- PR16-04 (run-log review): plain sleeps woke 37 to 82 ms late at the median, not 37 to 141 (the 141 was user-interactive QoS); fixed in the run log and EXPERIMENTS' E19 section.
- PR16-05 (run-log review): the retraction of #13's budget note went too far: the list frame's +1 is command-list growth (16 draws are 19 commands; the step is at 14 draws), and only the four-pass +2 was wgpu's per-pass work.
- PR16-06 (run-log review): the budget example used 67 and 2 + 1 + 1 + 3 unexplained: a first-layer column, the list-cost rule and the four passes' names are added, and the old budget is given as 56 per frame plus 23 per extra pass.
- PR16-07 (run-log review): windowed E19 looked 3× faster than headless with no reason: they end at different points (`present` returning, GPU done), and the display adds up to one refresh; time to photons is a next step.
- PR16-08 (run-log review): "reads as 'checkbox, checked'" sounded heard: it's AccessKit's mapping, and no screen reader was tried; "collapsed" is announced on Windows and iOS only.
- PR16-09 (run-log review, nits): stale pointers ("On exe1 unless marked Mac", the follow-up's Numbers), "and #14 to 6", AccessKit glossed, `report.py` exits on any JS stamp outside dispatch → apply, and the inherited-color paragraph folded into a footnote under the table.
- PR17-01 (press review): a link in two spans (`<Text onPress>See <Text weight={700}>logs</Text></Text>`) activated only when released on the span pressed, since `press_up` compared span indices: span flag bit 5 (press joins) marks a pressable span of the same pressable Text as the one before, `press_run` compares runs, and `a_pressable_span_presses_its_text` presses "fai", releases "led" (and back), and checks two adjacent links stay two.
- PR17-02 (press review): "the web's click rule" was React Aria's; the web clicks the nearest common ancestor of the press and the release. The rule stays; press.rs, the Built paragraph, the test and the PR body say whose it is, and DF-45 records the divergence.
- PR17-03 (press review): Click was offered on every button, link and check role, and on disabled pressables, where it did nothing: `a11y.rs` offers it on enabled pressables only, whatever the role, with no synthesized pointer fallback; `click_only_on_enabled_pressables` covers a disabled Pressable and a pointer-only checkbox View.
- PR17-04 (press review): modified Enter and Space didn't activate, where Chromium's do (Shift+Enter, the kit's Cmd+Enter to open a link in a new tab): they activate with the modifiers (Space: those at its key up), claims still win, and the Shift+Enter assertion flipped.
- PR17-05 (press review): dropping `key_press = None` from `set_focus` failed no test: Space down on Archive, Tab to the composer, Shift+Tab back, Space up now activates nothing, and Archive's `_pressed` is clear while away. The mutation fails it.
- PR17-06 (press review): a span press could outlive its span table, with out going to whichever Text owned the index by then: `Press` keeps the paragraph revision, a changed one cancels at the release (with the old revision), and the facade sends out or cancel to the Text that heard the press in (`pressTarget`); a bun case inserts a Text before the link mid-press.
- PR17-07 (press review): DF-42 to 44 collide with the focus-traps branch's; #17 keeps them (and DF-45) as it likely lands first, and the traps branch renumbers.
- PR17-08 (press review, nits): ARCHITECTURE.md §16 said protocol 6: it says 7, with press flags, pressable spans and `PRESS`/`ACTIVATE`.
- PR17-09 (press review, nits): an enabled Pressable inside a disabled one activates (React Native's rule, not the web's): the `disabled` JSDoc and the Built paragraph say so.
- PR17-10 (press review, nits): `onPressIn` and `onPressOut` are primary-button only, where the kit's web `onPointerDown`/`onPointerUp` hear any: their JSDoc says so.
- PR17-11 (press review, nits): a root `<Text onPress>` read as static text: it defaults to role `link` (a given role wins); the bun case checks both and the return to `text`.
- PR17-12 (press review, nits): a lone `onPressIn` or `onPressOut` made a Text pressable, swallowing its row's presses: only `onPress` does now, and the JSDoc says a Text's press-in and out need it; a bun case checks such a Text sends no press flags or pressable spans.
- PR18-01 (traps review, M1): a `focus()` sealed with the trap op (a layout effect's) became the restore target, so closing the dialog lost the focus: the focus is saved when the transaction begins, and `a_focus_while_opening_keeps_the_restore` checks closing returns to "Delete…".
- PR18-02 (traps review, M2): a Suspense-hidden modal stayed active (the app gated, Tab in the hidden dialog, the accessibility tree empty): a trap under `display: none` or `inert` is inactive, restores on hiding and activates and auto-focuses again when shown, as the web remounts; `a_hidden_trap_is_inactive` goes both ways.
- PR18-03 (traps review, m1): a Focus command was judged against the gate before the traps settled: one the traps allow applies at once (a text insert may follow), one they block is judged at the settle, and either beats a restore; `focus_commands_see_the_settled_traps` focuses into a modal opening with it and out of one closing with it.
- PR18-04 (traps review, m2): closing the menu and the dialog together sent blur 21, focus 14, blur 14, focus 1: the settle picks the final target (restore, command, auto-focus, mount, O1, each overriding the one before) and moves focus once; `closing_traps_together_moves_focus_once` checks the events, and a dialog closing as another opens goes straight to the new one, which inherits the old one's restore.
- PR18-05 (traps review, m3): a dialog toggled off and on went above its open menu modal, making its content live under the menu: a trap activating goes below the active traps inside it (`an_inner_modal_stays_on_top`).
- PR18-06 (traps review, m4): the comments now match DF-48, which is widened; an `autoFocus` node mounting into an active trap the focus is outside of now takes it (a wizard step loading in; `auto_focus_on_mount_into_a_trap`).
- PR18-07 (traps review, m5): tests for the gate following a layer opened and unowned later, a path node not hit (an inline modal), Shift+Tab from nothing, the accessibility tree after closing, and a bridge Layer opening in a later commit than its trap; restore order and the new rules each fail a test when mutated. Two are not tested: auto-focus order is equivalent either way (an outer trap's target inside an inner trap is the inner trap's target too), and the path node's inert check in the accessibility tree is removed, unreachable now that a hidden or inert trap is inactive.
- PR18-08 (traps review, nits): `reported`'s doc comment is back on it; the modal flag goes on the trap's first `dialog` or `alertdialog` (roles 17 and 18, new in protocol 8), else the trap; the gate reuses its buffers and the focus chain its Vec; Tab walks a hidden or inert subtree only down to owners of layers; the Built cost bullet has the deep-walk row.
- PR18-09 (lead, rebase on #17): protocol 8, DF-47 and DF-48 after #17's DF-42 to DF-46, and a press on a node turning inert or falling outside a new modal cancels with #17's `PRESS` cancel, Space's press too (`a_blocked_node_loses_its_press`), where this branch had filed it as a DF.
- PR19-01 (parts review, m1): the per-axis variant merge was untested (a `TRANSLATE_X` or `SCALE_Y` that copied the whole pair survived): `variants_merge_parts_per_axis` puts a hover `translateX` + `scaleX` beside a selected `rotate` over a base translate `[5, 7, 0, 0.1]`, then gives the selected one `translateY` + `scaleY`, then drops the hover. Each of the four whole-pair mutations now fails it (checked).
- PR19-02 (parts review, m2): quarter turns left f32 residue (rotate(π) had b = -8.7e-8, rotate(τ) 1.7e-7), so a 360deg `animate` or a chevron at 180deg stayed transformed and drawn unsnapped: `Parts::compose` snaps sine and cosine within 1e-6 of 0 or ±1. rotate(τ) composes to exactly identity, the transform record goes and the subtree snaps again; rotate(π) is exactly `[-1, 0, 0, -1]`. π/2 and 3π/2 are exact too but not `is_axis_aligned()`, which means b = c = 0 (they swap the axes). With the snap removed, both new tests fail. The 1e-6 tolerance covers every multiple of π up to 10π (five turns); from 11π on, the f32 angle's own rounding is larger, so a sixth consecutive full turn ends 1e-6 off (an app that spins forever should wrap its angle).
- PR19-03 (parts review, m3): a percentage in an RN list's translate sent `[1,0,0,1,NaN,NaN]` and native rejected the transaction: `transformMatrix` throws on a non-number translate or scale ("percentages go in style.translate (DF-49)"), bun test. DF-49 said no ported screen writes one; Marbre's native resolver does (the `roll-*` presets, `translateX: '50%'`): DF-49 now says the kit adapter must map those keys and presets to parts.
- PR19-04 (parts review, nit): lengths, percentages and angles parsed with `Number`, so "%" was 0%, "0x10%" 16% and "0x1deg" 1deg: a strict CSS-number pattern (sign, digits, fraction, exponent) replaces it; units are case-insensitive, as in CSS (bun tests).
- PR19-05 (parts review, nit): "a composition that overflows leaves the row unchanged" held for the row but not the base the variants resolve over: docs fix, not a validation. `validate` sees the host but not the base values, and a variant's scale over a valid base can still overflow at resolve time, so validation couldn't keep the promise either. The docs and `set_spatial`'s comment say only the row is guarded, and the base keeps the part.
- PR19-06 (parts review, nit): topic 7's Built paragraph gains a Cost line with the review's E15 numbers (load 22 to 30).
- PR20-01 (groups review, M1): Tab re-sorted the skip list at every group, so its cost grew with the square of the group count (1,000 groups of three: 0.8 to 10.6 ms per Tab in E15, about 2,030 allocations). The walk now collects the skips, sorts once at the end and filters the order with a binary search. The walks of one Tab share their buffers (`GroupWalk`). The same row is now 0.1 to 1.1 ms with 30 to 39 allocations. E15 gains the row "Tab, 1000 groups x 3" (the single-group and no-group rows stay), and the Built paragraph's Cost line covers many groups.
- PR20-02 (groups review, m1): an arrow's `KEY_DOWN` went to the member it focused, after the move and the activation. `KEY_DOWN` now goes to the node focused when the key went down (if still live), before any default action, as on the web: KEY_DOWN on the old target, then blur and focus, then (with selectOnFocus) ACTIVATE on the new member. This covers Tab too, which changes main: Tab's `KEY_DOWN` went to the node it focused. Enter's activation and an input's change or submit now follow the `KEY_DOWN`. `the_key_goes_to_the_node_it_leaves` checks an arrow and Tab.
- PR20-03 (groups review, m2): a focused member that was disabled but still focusable fell out of the Tab order (Tab jumped to the window's first stop, and ↓ did nothing). While it holds the focus it is a member and the group's stop: Tab leaves from it, and arrows move to the next or previous enabled member in tree order from it. `a_focused_disabled_member_is_the_stop` covers Tab, Shift+Tab, ↑, ↓, and Tab back in.
- PR20-04 (groups review, m3): selectOnFocus activated with the arrow's modifiers (Shift+↓ read as Shift+click). It now activates with none, as the web's `click()` does (`select_on_focus_activates_without_modifiers`). The docs line and the `selectOnFocus` JSDoc say so.
- PR20-05 (groups review, m4): three mutations survived. Tests now kill each one: `a_non_member_is_never_the_last_focused` (note_group_focus without `is_member`), `a_removed_group_is_forgotten` (Remove leaving `groups` untouched), and the event order in `the_key_goes_to_the_node_it_leaves` (activation before `set_focus`). I checked that each mutation fails its test, along with those for m2 and m3.
- PR20-06 (groups review, n1): `member_holding` stops at the first member on the path (`find_map`). Its comment says members don't nest, rather than "outermost".
- PR20-07 (groups review, n2): a group node left focusable stays its own Tab stop, as on the web. The Built paragraph says so.
- PR20-08 (groups review, n3): a `group` bit in `Interaction` spares Tab and group walks a `Ui::groups` lookup on each node. It isn't a `NodeFlags` bit because the header's flag byte is full (8 of 8) and the header is 16 bytes. `Interaction` stays 12 bytes.
- PR20-09 (groups review, n4): the Built paragraph says an outer selectOnFocus group's arrow into a nested group activates the inner group's stop.
- PR20-10 (groups review, n5): the group node reports AccessKit's orientation from its flags (horizontal or vertical; none for both). `the_orientation_is_in_the_accessibility_tree` covers all three.
- PR21-01 (animations review, M1): native keyed an animation by its position on the wire, so a falsy or policy-dropped entry restarted every entry after it (`[busy && fade, spin]`: `busy` going false restarted the spin). Each entry now carries its index in the author's list, counted before filtering (a u8 first in the entry; indices rise within a list, checked). The key is `index | trigger + 1 << 16 | block << 32` (u64): a variant's block is its position among the node's flattened blocks, sent or not (a u16 before the ANIMATIONS count), so 256+ variants no longer collide. The end event's index is the prop's index; `motionIndex` is gone (DF-57 closed). `entries_keep_their_phase_as_others_come_and_go` covers the list (the drop is the same op as a reduced-motion one) and a variant block appearing before a loop's; the facade test covers an empty block keeping the later block's position.
- PR21-02 (animations review, M2): a finished finite animation without a forwards fill was forgotten, so any re-sync replayed it. It now stays as a tombstone while its key is declared (not live, covers nothing, not sampled) and goes when its entry does or its variant stops applying. A re-send with the same key and keyframes updates in place (timing only while it runs, as CSS); changed keyframes restart it. `a_done_animation_never_replays_while_declared` covers (a) an app state restyling a held hover, (b) `[shake]` growing to `[shake, spin]`, (c) re-timing to 0 s and back (no replay, no second end) and a running one re-timed to 0 s ending once; `a_list_restarts_on_changed_keyframes_only` covers timing in place and keyframes restarting.
- PR21-03 (animations review, m1): a variant's `transition` replaces the node's list while it holds, as CSS and Marbre's web kit do; `"none"` or `{}` sends an empty list (TRANSITIONS bit with count 0). Leaving uses the base's list. `a_variant_transition_list_replaces_the_nodes` covers the jump during hover, leaving, and an empty list. DF-22's closing note says so; the reversed-transition shortening is DF-60.
- PR21-04 (animations review, m2): animations on a node that isn't drawn (display none, under a hidden ancestor, detached) no longer run, count as live or force frames; running ones end `cancelled`, and all start over when the node is drawn again, as CSS does. `Ui::drawn` walks the ancestors (exits will keep an exiting subtree drawn though detached; the hook comment is there), rechecked when the structure revision moves or a node gains motion. `animations_run_only_on_drawn_nodes` covers a spinner under a hidden parent and a detached node.
- PR21-05 (animations review, m3): any animation not yet done pins the parts it covers, finite ones included, until it ends. The allocations invariant's pulse is now 100 iterations and lands on boundaries with 0 allocations (pinning loops only: 332 allocations over the run).
- PR21-06 (animations review, m4): the facade throws on unknown keyframe keys, naming the key (`keyframes cannot animate "bg"`). Native's validation names the fault: frames out of order, offsets outside [0, 1], no frame, non-finite values, opacity range, unknown channel, easing and timing ranges (`malformed_keyframes_say_why`).
- PR21-07 (animations review, m5): an `enter` with a forwards or both fill holds over the node's value for good: documented in topic 7, the `enter` JSDoc and DF-58.
- PR21-08 (animations review, m6): native reports the environment once when a session starts (its first transaction), so a host created late learns reduced motion; later reports come only on change (`the_session_starts_with_the_environment`).
- PR21-09 (animations review, m7): delays in [-600, 600] s are accepted on both sides and start partway through, as CSS (`a_negative_delay_starts_partway`, and the facade's delay test).
- PR21-10 (animations review, nits): steps take CSS's before flag (jump-start and jump-both show 0 during a backwards-filled delay); the `AnimationEndEvt` JSDoc drops `removed` (a removed node reports nothing; `animate` resolves `removed` in JS on release); the Rust encoder checks its keyframes table index (`the_encoder_checks_its_keyframes_table`); `the_more_specific_variant_animation_wins` ranks two variants animating one property; topic 7 says a changed entry's keyframes restart it, not its position; DF-59 records a running `enter` continuing after reduced motion turns on.
- PR21-11 (lead, after review #21): variant animations were keyed by the block's position among the node's flattened blocks, so a falsy block (`_pressed: busy && {...}`), which flattening skips, still shifted the blocks after it and restarted their loops. The host now gives each animated `_` path (`_selected._hover`) a block number when it first sees it and keeps it for the node's life (`HostNode.variantBlocks`, reset with the id); no wire change. The `Variants` type takes falsy blocks. The bun test puts a conditional animated block before two animated ones and checks theirs stay 0 and 1 as it comes and goes (numbering by position fails it).
- PR22-01 (GPT-6 Astra review, P1): unmount cut running exits with `REMOVE`, but native may have finished one and freed its ids with the `EXIT_END` still on its way; the `REMOVE` of the freed id then failed validation and closed the session, and a second unmount repeated it. New op `END_EXIT` (0x05, protocol 12): it ends the exit if it still runs (removed) and does nothing once it has ended; a live node without an exit is rejected, and plain `REMOVE` stays strict. The facade's `endExits` sends it once per exit (`Exit.cut`), so a second unmount sends nothing. Tests: `end_exit_is_idempotent`; bun "an exit that ends as unmount cuts it recycles once" and the unmount test run twice; the fixture carries an `END_EXIT`.
- PR22-02 (GPT-6 Astra review, P2): an exit's omitted frames sampled what ran under it each frame, so an interrupted linear enter (0 → 1 over 1 s, removed at 200 ms, fade out over 500 ms) rose to 0.24 at 300 ms instead of falling to 0.16. The detach now captures what shows (`row_sample`, before the hover leaves) and the exit's records sample from it for the channels they animate (`Running.from`), as Framer's `AnimatePresence` does. `an_exit_starts_from_what_showed` covers an interrupted enter, an opacity transition and a hover animation (each fails without the capture: 0.24, 0.56, and 0.8 against 0.16).
- PR22-03 (GPT-6 Astra review, P2): validation modelled a `REMOVE` of an exit's root as freeing that node only, while execution frees the subtree: `REMOVE(0), PLACE(root, 1)` with 1 inside exiting 0 passed and put the dead id 1 in the root list. The overlay now frees the root's subtree as the batch left it (host links it kept and its own placements), and `END_EXIT` does the same. Execution also cuts an exit started in the same transaction that could not run (skipped) when its root is removed, rather than freeing the root twice. Tests: `a_cut_exit_frees_its_subtree_in_validation`, and a same-transaction detach and remove in `removes_and_skips_end_at_once`.
- PR22-04 (GPT-6 Astra review, P2): the overlay marked a detached exit root as detached, while execution keeps it in its parent's list, so `ANIMATION(B, exit), DETACH(B), CREATE(D), PLACE(P, D, before=B)` failed in one transaction but passed across two. The overlay now keeps the parent when the exit will run, and records exit state in one map (declared, cleared, started). For validation to know whether it runs, native decides by structure alone at the detach (in the tree, not a List row); an exit hidden at its detach starts and ends skipped on the next frame, like one hidden while it runs. Tests: `a_new_sibling_places_by_the_named_one` (same transaction) and `a_hidden_exit_is_skipped` (a sibling placed by the hidden root in its detach's transaction).
- PR22-05 (GPT-6 Astra review, P2): the allocation test declared exits outside its measurement; with the bridge's real transaction (`ANIMATION` then `DETACH`) the exit's transaction allocated 5 times (the review), 4 once the overlay stopped writing the root's parent link (PR22-04). Keyframes now keep emptied records' lists (up to 64) for new records, and the overlay keeps exit state in one map; the transaction allocates twice (validation's overlay entry, as any detach, and the declaration's copy), against a plain detach's 3 (overlay entry, and 2 as the parent's child list moves). The test (`exit_transactions_allocate_twice`) measures the whole transaction and asserts at most 2; the render stays one above a plain detach's (the fade's pinned layer).
- PR22-06 (GPT-6 Astra re-check, P2): since PR22-04 an exit hidden at its detach started and ended skipped only in `finish_exits`, inside `render`; a minimized window (zero-sized surface, `app.rs` returns before rendering) held the subtree natively and its parked ids in JS until restored. The parent is still kept through validation and apply, but exits under `display: none` now end (skipped) at the end of the transaction, after its styles (`end_hidden_exits`), frames or not; a variant hiding one between transactions still ends it on the next frame. A visible exit in a window drawing no frames waits for the first frame after it shows again, as tweens do (the Built paragraph says so). Test: `a_hidden_exit_is_skipped` now draws no frame around the commits under test (fails without the call).
- PR22-07 (GPT-6 Astra re-check, P2): each exit cut in validation rebuilt a parent index from every link the batch had set, freed nodes included: 1,000 cuts in one unmount looked at 499,500 entries (about 120 ms; 2.2 s for 5,000). The overlay now builds the index once, at the batch's first cut, keeps it as the batch places (`Overlay::link`), and walks only the cut's subtree (host children plus current placements; `parents` tells stale entries apart), reusing one list. Tests: `a_bulk_cut_walks_each_subtree_once` (1,000 `END_EXIT`s, asserts 3,000 nodes and links looked at, and prints the time; 1,002,000 with a rebuild per cut), and `a_cut_exit_frees_its_subtree_in_validation` gains a node placed after an earlier cut (fails if placing skips the index).
- PR22-08 (GPT-6 Astra review at 41ea552, P2): hidden exits ended before the transaction's last restyle. A subtree shown only by `_focusWithin` (base `display: none`) lost the focus at its detach, the final restyle hid it, and the transaction returned with the exit live and no `EXIT_END`, holding its ids until a frame. `end_hidden_exits` now runs after that restyle. Test: `an_exit_hidden_by_losing_its_focus_is_skipped` (fails with the old order).
- PR22-09 (same review, P2): beyond validation's subtree walks (PR22-07), ending exits in bulk scanned: each cut looked an exit up in a list (validation and execution), removed it with a `retain` over all exits, searched the others for ones inside, and filtered every queued event; 1,000 cuts made about 2,000,000 visits (4.6 ms), 5,000 about 50,000,000 (69 ms). Running and skipped exits are now sorted sets (`BTreeSet`), an exit inside is found by looking up each freed node, ends found in a frame are collected in one pass, and the events of freed nodes are filtered once, when events are taken (`Ui::freed`). Test: `a_bulk_cut_is_linear` (was `a_bulk_cut_walks_each_subtree_once`) counts validation's walk (3 per toast) and every visit of the ending path, cuts, freed nodes and queued events (4 per toast), for `END_EXIT` and `REMOVE`.
- PR22-10 (same review, P2): text inside an exit stayed selectable: the subtree is attached while it exits, so a selection inside survived (Copy still copied the removed text), and one around it kept copying an exiting paragraph. Selection validation now treats an exiting ancestor as detached, and the domain's texts skip exiting subtrees. Test: `an_exiting_text_leaves_the_selection` (domain, endpoint and middle paragraph exiting; fails without either check).
- PR25-01 (GPT-6 Astra review at 0a72b30, P2): `measure()` reported rows a native list hides (a retained row whose item was removed) and their descendants as measured, with zero-size boxes, where they should be `null`. `displayed` now also stops at a row its list hides (`Host::list_row_shown`), and layout events follow the same rule: a hidden node's box is not sent, and a new listener on one waits until it shows. Test: `hidden_list_rows_are_not_measured` (the review's repro, plus the layout event).
