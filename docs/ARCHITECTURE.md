# Craie architecture

This document records, for each subsystem, four things:

- **Current**: what the code in this repository does today.
- **Target**: the shape we are building toward.
- **Experiments**: alternatives we want to measure before or after the
  target lands. An experiment is a proposal until `EXPERIMENTS.md`
  records a result.
- **Decisions**: settled choices, with the reason. A later decision
  overrides an earlier one.

Measurements live in `EXPERIMENTS.md`. The vision and the long-form
target architecture live in Notion ("Craie — Vision" and "Craie —
Target Architecture"). This file is the operational summary the code
is held to.

Craie is a compact, data-oriented rendering runtime for React and other
declarative frontends, written in Rust. Marbre, a desktop agentic
application, is the first consumer and drives the next milestone.

## Principles that bind decisions

1. **One authoritative store per fact.** A derived copy may exist only
   when it carries the revision that proves it valid and a benchmark
   shows the recompute cost. Per-frame scratch is not state.
2. **Specificity over generality.** Keep an abstraction only when Craie
   has more than one real implementation behind it.
3. **Data-oriented storage.** Dense typed stores, generation-checked
   handles, hot/cold split, local coordinates, pooled spans.
4. **Costs are visible.** Allocations, copies, regenerated ranges,
   uploads, and bridge bytes are counted and asserted in tests.
5. **Retain prepared work.** Structural, geometry, metric, paint,
   spatial, and residency changes invalidate only what depends on them.
6. **Every fast path has an oracle.** A specialized kernel must equal
   the general path. An incremental update must equal a clean rebuild.
7. **Always benchmark.** A good idea ships when it measures better than
   the current state and the alternatives.
8. **Existing code is material, not a constraint.** We are in an early
   experimental phase. Anything can change.

## Layer map

Target crate graph. Arrows point from dependent to dependency.

```
craie-node (N-API)          harness/* (dev only)
      |                           |
      v                           v
craie-platform-winit  ----->  craie-ui  ----->  craie-text
      |                           |                 |
      v                           v                 |
craie-render (wgpu)  ------>  craie-scene  <--------+
                                  |
                                  v
                              craie-core
```

The renderer reads the scene's pools and tables; the scene knows
nothing of wgpu, so another submitter (E11) can reuse it.

- `craie-core`: handles, generations, geometry, revisions, dirty queues,
  span pool, counters. No knowledge of React, winit, wgpu, fonts, or
  Taffy.
- `craie-scene`: retained drawing chunks, transform/clip/paint tables,
  resources and residency, draw-order derivation.
- `craie-render`: wgpu device, pipelines, buffers, uploads, passes.
- `craie-text`: Unicode analysis, font instances, shaping, paragraph
  layout, editing, selection, glyph raster behind `RasterId`.
- `craie-ui`: host tree, mutation executor, layout, lists and
  scrolling, interaction, animation driver, accessibility projection.
- `craie-platform-winit`: window, events, clipboard, IME, font
  discovery, image decoding, AccessKit adapter.
- `craie-node`: N-API bridge between the React worker and the UI thread.
- `craie-vector`, `craie-svg`: appear when lyon lands.

Current state: the crate graph above, without `vector` and `svg`. The
file map is:

```
crates/core/            craie-core: no dependencies
  geom.rs               Point, Size, Rect, RectPx, Affine
  span.rs               SpanPool<T>: capacity-classed lists, dirty ranges
  dirty.rs              DirtyQueue (bitset dedupe), DirtyRanges
  rev.rs counters.rs    Rev stamps, cost Counters
  rng.rs                pinned-seed xorshift for model tests
crates/scene/           craie-scene: core, bytemuck, etagere
  prim.rs               RectInstance, GlyphInstance, Color, Segment
  chunk.rs              Chunk, ChunkWriter, Placement, PaintSlot
  space.rs              transform records (world derived), clip records
  scene.rs              pools, placements, draw order, layers, prepare()
  atlas.rs              RasterAtlas: RasterId, residency, pages, eviction
crates/render/          craie-render: wgpu
  lib.rs                storage-buffer mirrors, draw list, opacity layers
  shaders/              scene.wgsl (rects + glyphs), composite.wgsl
crates/text/            craie-text: HarfRust, skrifa, unicode-*, Swash
  paragraph.rs          owned paragraph: analysis, shaping, lines, carets
  fonts.rs              FontStore, FontSource, RawFonts, pinned fonts
  lib.rs cache.rs       engine, emit into chunks, GlyphKey -> RasterId
                        (Parley for inputs until step 3b)
crates/ui/              craie-ui: core, scene, text, Taffy, accesskit
  mutation.rs           Mutation enum, Transaction + direct-API builder
  wire.rs               CRW2 decode/encode
  executor.rs           validate + apply: revisions, dirty queues
  host.rs               16-byte headers, per-node stores, child spans
  layout.rs             Taffy over host rows; moved/resized tracking
  scene_sync.rs         host + layout -> chunks, placements, records
  dispatch.rs           hit testing, focus, pointer capture, editing
  ui.rs                 facade; a11y.rs, input.rs, surface.rs, events.rs
  bridge.rs platform.rs Session (commit queue, outbox), platform contract
crates/platform-winit/  winit 0.30 driver, Window, HostApp, clipboard,
                        fonts.rs (SystemFonts: fontique behind FontSource)
crates/node/            N-API: NativeHost, NativeClient
harness/invariants/     craie-harness: invariant, cost, graph tests; E01
                        (Parley oracle), E10, E14
assets/fonts/           pinned test fonts (OFL), `pinned-fonts` feature
```

---

## 1. Workspace and crates

**Current.** The crate graph of the layer map (core, scene, render,
text, ui, platform-winit, node) plus `harness/invariants`. `craie-core`
has no dependencies. `scripts/ci.sh` runs `cargo check --target
wasm32-unknown-unknown` on core, scene, render, and text, and the
harness asserts the layer map and the release graphs.

**Target.** The crate graph above. Desktop-only facilities never enter
`core`, `scene`, `render`, or `text`. CI runs `cargo check --target
wasm32-unknown-unknown` on those four crates. Size tracking starts when
a browser milestone begins.

**Experiments.** None. Crate granularity is a structural decision, not
a measurement.

**Decisions.**
- Split now and port the existing code in place. The code is small and
  the boundaries enforce the no-winit-in-core rule from day one.
- Create only crates with content. `vector` and `svg` appear with lyon.
- The verification harness is a set of workspace crates under
  `harness/`. Reference libraries (Taffy, Parley, HarfBuzz, browser
  renderers) enter only as dev-dependencies. CI asserts they are absent
  from release dependency graphs.
- `craie-render` depends on `craie-scene`, not the reverse (2026-09-23).
  The scene is portable data; the renderer is one consumer of it.
- A release build is per package: `cargo build --release -p craie-node`
  (as `scripts/build-addon.sh`). A `--workspace` build unifies features
  with the harness and turns on `pinned-fonts`; it is not a release
  build. The harness asserts both (step 3a).

## 2. Wire and mutation executor

**Current.** CRW2 as targeted, minus the animation family (step 4).
The list family (0x90) carries list configuration (overscan, fallback
extent, row templates), item splices (11 bytes per item: template
u16, text length u32, identity u32, flags u8, borrowed from the buffer),
a row's item index, and a scroll container's anchor policy. Validation
tracks each list's item count and identities through the batch (every
non-NIL identity once per list; creating or removing a node resets
both) and rejects unknown item flag bits and partial descriptions.
Identities are adversarial input: validation and the index sort and
binary-search them and never hash them. Header: magic, version, flags, seq, then counts and
per-transaction tables for strings, layout styles (u64 presence mask +
positional fields), and text spans (16 bytes each). Ops are u8-tagged,
grouped by family in the high nibble. `wire::decode` yields a
`Transaction` of `Mutation`s; the Rust builder produces the same type;
`Ui::execute` validates the whole transaction, then applies it with no
callbacks. Both encoders intern styles and span lists per transaction.
The decoder rejects unknown style tags and mask bits; node ids are
bounded (2^24) and dense (a batch may reach only the slots in use plus
the nodes it creates, plus 4,096).
Detach stays a structure op: React unlinks a deleted subtree in the
mutation phase and frees each node later.

**Target.** CRW2. The decoder produces a semantic mutation enum. One
executor consumes that enum from the JS transaction and from the Rust
direct API. The executor validates the whole transaction, applies it
atomically, produces dirty work, and advances revisions. No callbacks
run during application.

Op families: structure (create, place, remove), layout style (per-node
layout inputs), spatial (transform, opacity), paint (fill, border,
radius, colors), text (paragraph spans), semantics (role, label),
interaction (listener mask, focusable), list (item count, estimates,
item descriptions), animation (transitions, animate command), payload
(typed-array bytes for surfaces), command (focus, blur, setText,
scrollTo).

**Experiments.**
- Per-transaction style intern table versus inline styles on every
  set_style. Measure wire bytes, encode time, decode time on the mount
  and stream workloads.

**Decisions.**
- One breaking redesign during the split. Golden fixtures regenerate
  once.
- Transform and opacity travel in their own spatial op and never touch
  layout inputs. The facade splits the style object at commit time.
- Styles are stored per node natively. Interning, if kept, is transport
  compression only and dies with the transaction.
- The `hidden` op is removed. `display: none` is the one way to hide.
- Synchronous JS painters are removed. Custom content is native
  components and surface records fed by payload ops.
- The ack-gated id free list is removed. JS recycles ids immediately.
  Outbound events carry the node generation and JS drops stale ones.
  The ack remains only to resolve `flush()`.
- Span lists intern per transaction like styles (2026-09-23): most
  paragraphs share one style, and without interning CRW2 cost 9% more
  bytes than CRW1 on the 5k-row mount (see `EXPERIMENTS.md`).

## 3. Host tree and storage

**Current.** As targeted. `NodeHeader` is 16 bytes: parent, child
span, kind, flags, generation. Dense per-node stores: `layout`
(`taffy::Style`, 240 bytes), `spatial` (transform, opacity, scroll
offset), `paint`, `paragraphs` (UTF-8 + span list), `interaction`
(listeners, focusable, role). Labels and surface payloads are id-keyed
maps. The nine revisions live on the host; dirty queues (layout,
content, paint, spatial, semantic) are `DirtyQueue`s.

**Target.** The same arena shape with per-usage stores:

```
nodes[]            header: parent, kind, generation, flags, child span
layout_inputs[]    dense per-node layout row (no shared records)
spatial[]          local transform, opacity, clip ref
paint[]            fill, border, radius
text_nodes[]       paragraph: UTF-8 + span list
interaction[]      listener mask, focusable, role
```

Children are a span (offset, len) into one capacity-classed span pool
shared with scene chunks. A leaf pays nothing for children.

Revisions: `structure_rev`, `layout_input_rev`, `text_content_rev`,
`text_metrics_rev`, `paint_rev`, `transform_rev`, `clip_rev`,
`semantic_rev`, `resource_rev`. Dirty queues schedule work; revisions
prove validity.

**Experiments.**
- E10: span pool with capacity classes versus the current Vec side
  table, on the same interface, on deep/wide trees, tiny containers,
  huge lists, and reorder churn. Measure bytes per node, allocations,
  traversal cost.

**Decisions.**
- Layout inputs are per-node rows. This is what lets the animation
  driver write a tween value for one node without a second copy.
- One span pool serves both child lists and chunk ranges.
- The paragraph style is the span list. `TextRow` loses font size and
  color; span zero carries the base style.
- Dense per-node stores cost about 348 bytes per node today, 240 of
  them the `taffy::Style` row (2026-09-23). A compact Craie-owned row
  replaces it with the owned flex engine (step 6).

## 4. Layout

**Current.** Taffy 0.14 through its low-level traits, reading each
node's own layout row. Results land in `Layouts` (cache, unrounded
layout, final rect, content-box offset); finalize records nodes whose
origin moved, whose size changed, and whose scroll extent changed. A
layout pass runs only when the layout queue is non-empty or the
viewport changed, so an unchanged frame does zero layouts. React
Native defaults hold natively: a node with no style, a partial style,
or a reset style is a flex column whose children do not shrink
(`flexShrink: 0`; `host::default_style`). Cold layout
of 10k nodes costs 217 to 567 ms; one streaming append costs 0.65 ms.

**Target.** A Craie-owned engine over `layout_inputs[]`, the child span
pool, intrinsic measures, a layout cache, and results (relative
position, border-box size, content-box offsets, overflow extent,
baseline). Supported domain: flexbox, CSS grid, and block-level boxes
with margin collapse. No inline formatting context. Styles classify
into kernels (fixed box, simple row/column, non-wrapping flex, general
flex, grid, block, absolute, intrinsic leaf). Every kernel must equal
the general path.

Public style API: a typed object with CSS property names in camelCase
(`display`, `flexDirection`, `gridTemplateColumns`, `padding`). A
`View` defaults to `display: flex`, column direction, stretch, and
`flexShrink: 0`, as in React Native. `block` and `grid` are explicit values.

**Experiments.**
- E05: specialized kernels versus Taffy on generated trees with
  percentages, baselines, min/max, intrinsic sizes, overflow.

**Decisions.**
- Virtualize first. The measured cold cost comes from visiting every
  node, and a virtualized list removes it without replacing Taffy.
- Own flex, then block, then grid. Taffy stays the production general
  path for each algorithm until the owned one passes its differential
  suite against Taffy in the harness. Taffy may ship for grid for a
  long time.
- Block means block-level boxes only. Inline content is always a Text
  paragraph node.
- RN defaults stay so Marbre's cross-platform kit needs no
  normalization on Craie.

## 5. Text

**Current.** Text nodes lay out through the owned paragraph
(`craie-text/src/paragraph.rs`, step 3a). `Shaper::shape` runs Unicode
analysis (graphemes, UAX #14 break opportunities, bidi levels with a
fast path when no character needs the algorithm, scripts), resolves a
font per grapheme (the span's primary instance when it covers the
whole grapheme, else the first fallback candidate that does), splits
items on level, script, font, and size, and shapes each item with
HarfRust 0.12 through a cached shape plan per (instance, direction,
script). skrifa gives the metrics. A cluster starts at a grapheme
(UAX #29, with GB9c conjuncts): HarfRust keeps CR and LF apart, and a
CRLF must be one cluster and one hard break. A `Paragraph` keeps runs
(text range, instance, size, level, metrics), the glyph placement store
(28 bytes per glyph: id, span index, cluster byte, advance, offsets,
pen x, y), lines, visual segments (run, glyphs, text, L1 level), and
one analysis byte per text byte (break opportunity, paragraph
direction, L1 classes, cluster flags). The glyph placement store is
the only glyph position store. Emission draws at pen x + dx. Hit
testing, carets, and selection rectangles take cluster edges from it:
the pen x of a cluster's first placed glyph, and the pen after its
last. No other position is kept or summed. The glyph cache keeps
bitmap geometry per `GlyphKey` (instance, glyph, size bits, subpixel)
and no positions. A glyph's span index is its paint slot, so a color
change patches a paint record and touches no placement. `rewrap` lays
an already shaped paragraph out at another width over engine scratch:
Parley's greedy line breaking and line metrics under UAX #14
(overflowing whitespace hangs and never breaks by itself, so it stays
on the line a following hard break closes; no-break spaces are
content),
then UAX #9 L1 and L2 per line, in place. A segment whose L1 level has
another direction than its run places its clusters in reverse, and the
mapping reads that direction. A width change does no shaping and, once
the stores have grown, no allocation. Measured against Parley in E01:
line breaks, cluster maps, byte-to-cluster queries, and drawn glyphs
(id, cluster, font) equal on every oracle case; the known differences,
each with its own test, are Craie's L1 at soft line ends, GB9c
graphemes, one break for CRLF and one after NEL, unbroken no-break
spaces, no break inside a marked grapheme, and one font per
grapheme. A retained paragraph holds 0.29 to
0.75 times Parley's bytes. The Text default family is the engine's
`default_family`, `system-ui` (Decisions). INPUT nodes still hold a Parley
`PlainEditor` each until step 3b. Its contexts are made on first use,
and its glyphs go through the same font store and glyph cache. `TextInput` is uncontrolled (`value`
is sent once at mount; `setText` replaces the text). Editor reshapes
count where Parley shapes: each edit, preedit, commit, and finished
composition, and a dirty layout refreshed before navigation (Parley
pinned to 0.11.1). Validation bounds font sizes to `MAX_FONT_SIZE`
(2048 logical points). A glyph larger than an atlas page renders: it
is rasterized at a smaller size that fits, and its quad draws the
bitmap scaled up (softer, never missing). The cache keeps the raster
size for re-rasterization. Glyph instances are chunk-local; their
subpixel buckets are relative to the chunk origin, which the renderer
snaps to the device-pixel grid.

**Target.** Craie owns every persistent representation:

```
UTF-8 + spans -> Unicode analysis -> font resolution + fallback
  -> shaping (HarfRust) -> ShapeRuns/clusters -> lines + placements
  -> glyph range in a scene chunk
```

- Storage: UTF-8 per paragraph with style spans over byte ranges.
  Explicit conversions to scalars, graphemes, UTF-16, and clusters.
- Unicode: `unicode-bidi`, `unicode-linebreak`, and segmentation crates
  as the first kernels. They double as oracles for E02.
- Shaping: HarfRust over `skrifa`/`read-fonts` font data, writing into
  Craie run storage.
- Paragraph: wrapping, lines, alignment, metrics, visual order, caret
  mapping, hit testing, selection geometry.
- One glyph placement store. Hit testing and rendering read the same
  placements. Color lives in the paint record, so a color change
  touches no placement.
- Editing: caret, selection, undo, IME composition state on a plain
  UTF-8 buffer per paragraph. Native owns the text. `TextInput` is
  uncontrolled: `value` is initial, `setText` is a command,
  `onChangeText` reports edits.
- Selection: a `selectable` prop on a View makes its paragraph
  descendants one selection domain. Copy yields plain text in tree
  order.
- Raster: Swash stays as the raster kernel behind `RasterId`.
- Nested `<Text>` flattens to spans of one paragraph node in the
  reconciler. Per-span events map through cluster ranges.

**Experiments.**
- E01: owned paragraph runtime versus Parley on pinned multilingual,
  styled, wrapped, editable cases. Measure correctness, retained bytes,
  allocations, cold and sparse-update cost.
- E02: generated Unicode tables versus the unicode-* crates.
- E03: HarfRust as-is versus an internalized shaping kernel.
- E04: incremental append/edit reflow versus fresh layout.
- E08: shared glyph/path coverage so Swash can leave.

**Decisions.**
- Own paragraph layout and editing over HarfRust and skrifa. Parley
  leaves production and stays in the harness as an oracle.
- Plain UTF-8 per paragraph. A rope or piece table can replace it
  behind the same mapping API if a large editor arrives.
- Cross-node read-only selection lands with owned text.
- Uncontrolled inputs only. Three copies of the input text (native,
  JS mirror, React state) is the classic desync bug.
- The Text default family is `system-ui` (2026-09-23, step 3a), React
  Native's default: RN draws `Text` without a `fontFamily` in the
  platform system font (San Francisco on iOS and macOS, Roboto on
  Android), and react-native-web in the `system-ui` stack. Parley's
  default was `sans-serif` (Helvetica on macOS), so glyphs, widths,
  and line breaks change against step 2. On the same font the owned
  engine's first draw is faster than step 2 (E01); SF itself costs more
  rasters than Helvetica. `TextEngine::default_family` sets another
  default (framebench `--family`).

## 6. Fonts

**Current.** `craie-text/src/fonts.rs` owns font identity: a
`FontStore` interns faces (`FontFaceId`, with units per em, an ASCII
coverage mask, a coverage cache for other characters, and HarfRust
shaping data built on first use) and instances (`FontInstanceId`: face,
normalized variation coordinates, synthesis). A face's key is its byte
identity and index: fontique blob ids from fontique's process-wide
counter, and `RawFonts` ids from its own process-wide counter at or
above `RAW_ID_BASE` (2^63), so a replaced source never reaches another
source's faces. Discovery and fallback come through the `FontSource`
trait (`select(family, attrs)`, `fallback(cluster, script, attrs,
emoji)`), which returns font bytes in priority order: every face that
covers the cluster's first character, up to and including the first
that covers the whole cluster. `emoji` is UTS #51 presentation
(Emoji_Presentation from the Unicode 17 tables, VS15 and VS16).
A custom source takes byte ids from fontique's or `RawFonts`'s
counter. `RawFonts` is the
byte-only source (browser profiles, tests): family by name, nearest
weight and italic with synthesis, fallback by coverage in registration
order. The `pinned-fonts` feature embeds the harness fonts from
`assets/fonts` (Noto Sans regular, bold, and italic, Arabic, Hebrew,
Devanagari, a JP subset, Symbols 2, and a monochrome emoji subset,
under OFL). Tests and the harness lay text out on them. On desktop,
`craie-platform-winit/src/fonts.rs` implements `FontSource` over
fontique: the emoji family first for emoji-presentation clusters, then
the script's fallback list, then a last resort in a stable order (a
per-platform priority list, then every family by name), each step only
while no face covers the whole cluster, with no cap on candidates. The platform
installs it as the default source at startup. The engine caches the
primary instance per (family, attrs) and the source's candidates per
(script, attrs, emoji) and cluster; each cluster takes the first
candidate that covers all of it, so the choice depends on the cluster
alone. Until step 3b, Parley (for inputs) still brings fontique into
craie-text's graph.

**Target.** `FontFaceId` and `FontInstanceId` (face, variation coords,
synthesis, features) owned by `craie-text`. Desktop discovery and
fallback come through the platform contract. Browser profiles pass raw
font data. The scene renderer never requires system font discovery.

**Experiments.** None yet.

**Decisions.**
- fontique stays as the desktop enumeration and fallback kernel,
  reachable only through the platform adapter.

## 7. Lists and scrolling

**Current.** `overflow: scroll` on a View makes it a scroll container.
Wheel deltas scroll the nearest scrollable ancestor, clamped to the
content extent. A `List` node (step 2) virtualizes inside a scroll
container:

- The list owns its item count, item descriptions, and one extent per
  item in a Fenwick store (`core::extents`, f64 prefix sums and f64
  deltas): offsets, the item at an offset, and updates are O(log n); a
  splice rebuilds it in O(n), except a pure append, which extends the
  tree in O(k log n) (streaming appends to a 1M list take microseconds).
  The list's row gap enters offsets and
  lookups arithmetically (a prefix of k items holds k gaps), not as a
  second store; a percentage gap resolves against a definite content
  height, else the list sizes without it and places rows with it
  resolved against that size clamped by min/max, as flex does. About
  29 bytes per item with the identity index; 2^24 items at most.
- Items have identity: the bridge interns each item's React key to a
  u32, and each list keeps a sorted index of its identities (4 bytes
  per item, merge-updated per splice). Identity keeps
  a scroll anchor and a focused row on their item. What proves a
  measured height still valid is the `unchanged` flag: the bridge sets
  it when the item is the same (immutable) object it removed under
  that key, so a move keeps its measurement and an edit, however its
  estimate inputs compare, is estimated again.
- Estimates are native: each row template gives a fixed extent, a
  horizontal inset, and a font size; per-size metrics (average advance,
  line height) come from shaping a sample once. An item's estimate is
  the fixed extent plus its text length wrapped at the list width. A
  splice estimates new items at once when the width is known.
- The list is a leaf to its parent. Its content is its rendered rows:
  children tagged with an item index, each laid out at the list's
  content width and placed at its item offset. The final layout pass
  records their heights as measurements; size probes record nothing.
  Both use the content-box width Taffy resolves (padding, border,
  min/max, percentages), so a probe and the final layout agree.
  A width change forgets measurements (they were made at another
  width); rendered rows measure again in the same pass. Rows with no
  index, a duplicate index, or one past the end are hidden, from layout
  and from accessibility alike (`Host::list_row_shown`). A new fallback
  or new templates re-estimate unmeasured items.
- After layout and scroll, each list reports the item range around
  its viewport (the nearest vertical scroll container, else the
  window) as an event, with hysteresis: it reports again only when the
  viewport plus half the overscan leaves the reported range, and then
  asks for the viewport plus the whole overscan. The viewport maps into
  the list through the transforms paint and hit testing use (each
  node's origin and transform, and scroll offsets in between). The
  event names the focused row's item by index and identity, and the
  list's splice revision; a list reports again after every splice.
  React keeps the focused item's row by key and applies ranges only
  from the current revision.
- Anchoring: after every frame each scroller captures its anchor (the
  top visible item and its offset from the viewport top, and whether
  it is at its end). After a layout pass, `keep-visible` scrolls so the
  anchor item keeps its place; `stick-to-end` holds the end when it was
  there. A splice moves the anchor with its item: to its new place
  when it moved, to the splice start only when it was removed.
  Positions go through the same transforms as the range; the anchor is
  the visually top item and its visually top edge, so a flipped list
  anchors its far end. Explicit
  `ScrollTo`
  commands in the same batch win. Anchor corrections are motion for the
  snap policy (§8).
- The React `List` diffs `items` by identity into one splice (common
  prefix and suffix), keys rows by `keyOf`, and renders the reported
  range. Rows report their position in the set to assistive technology.

**Target.** A native list node inside a ScrollView.

- The list owns item count, per-item extents, and the visible range.
  It emits a visible-range event and React renders only those rows.
- Extents start as native estimates computed from a simplified item
  description sent in the list op (text length, font instance, width
  class). Measured heights replace estimates once a row renders.
- The ScrollView owns the scroll offset and the anchor policy:
  `keep-visible` by default (the top visible item stays put when
  extents above it change), `stick-to-end` opt-in (holds the bottom
  when already at the end). Lists report extent deltas.
- Content before and after a list shares the same scroller.
- Off-viewport focus, selection, and accessibility remain native
  concerns of the list.

**Experiments.**
- Native estimator versus a JS estimator fed by per-font metrics, on
  large message threads. Measure first-frame anchoring error, bridge
  bytes, and estimate cost.

**Decisions.**
- Native remembers measured heights; native computes estimates. No
  metrics or extents copy lives in JS.
- The list is not its own scroller. Wrapping it in a ScrollView is
  explicit.
- A scroll offset re-clamps when content shrinks below it
  (2026-09-23), as browsers do.
- Native estimator (2026-09-23, E14). Mean estimate error is 2.4% (p95
  25%, from wrap boundaries); after a jump into unmeasured items the
  top item holds and the viewport bottom moves 14 pt once. Ten bytes
  per item cross the bridge. A JS estimator was not built: it would
  need a copy of font metrics in JS, which the first decision rules out.
- The anchor is one list item per scroller, taken from the lowest list
  id that shows an item. Content outside lists does not anchor.

## 8. Scene

**Current.** As targeted, except images, paths, meshes, and group
opacity by multiply-through (only isolated layers exist). One chunk per
node (id = node id) in `RectInstance` (40 B) and `GlyphInstance` (20 B)
pools plus a paint pool, with up to four same-kind segments in paint
order. A placement table (offset, transform record, clip) positions
each chunk. Transform records exist for the window root, scroll
content, and transformed subtrees; all other nodes draw in their
nearest record's space at an offset. The draw order, layer table,
record evaluation order, and clip records are rebuilt by one walk when
structure or clip topology changes; layout moves revisit only moved
subtrees. Chunks build on demand within half a viewport of the screen.
`prepare` culls, merges contiguous segments, and wraps opacity layers.

**Target.** A persistent retained drawing representation.

```
Scene
 ├── chunks[]        paragraph, icon, vector object, surface, UI subtree
 ├── transforms[]    local -> parent, shared by ranges
 ├── clips[]         parent, shape (rect, rounded rect, path), transform
 ├── paints[]        colors, gradients
 ├── resources       ImageId, RasterId, PathId, MeshId + residency
 └── draw order      derived from tree order + z, keyed on structure_rev
```

- Primitives live in specialized arrays: `RectInstance[]`,
  `GlyphInstance[]`, `ImageInstance[]`, `PathRecord[]`, `Mesh2D[]`.
- All geometry is local. A translation patches one transform record.
- A chunk stays unchanged while its transform moves.
- Group opacity: an isolated layer from a pooled texture set when
  children overlap, multiply-through when the subtree is provably
  non-overlapping. Both paths must match within tolerance.
- Surfaces: one host node, one chunk, many instances, fed by payload
  ops. A surface participates in layout, clipping, z order, events, and
  semantics.

**Experiments.**
- E06: prepared chunks and shared tables versus the current rebuild,
  on scroll, pan, recolor, reorder. Measure CPU preparation, bytes
  patched and uploaded, draw calls.
- Isolated layer always versus isolated-when-overlapping. Find the
  overlap count or area threshold where the layer wins.
- T06: per-chunk resource dependency lists versus scanning glyph ranges
  each frame.

**Decisions.**
- Chunk, transform, clip, and paint records land during the split,
  because owned paragraph output must target them.
- Draw order is a derived cache rebuilt when `structure_rev` changes,
  not incrementally patched state. The measured walk is cheap.
- Transform records exist only for the window root, scroll content,
  and transformed subtrees; every other chunk is an offset in its
  nearest record's space (2026-09-23). A scroll patches one record and
  uploads 32 bytes; a layout move patches placements of moved subtrees.
- Snapping is a policy of the transform record (2026-09-23, revised
  after review rounds 1 to 3). The window root snaps chunk origins and
  rect edges to device pixels (ties to even, as WGSL `round` lowers),
  so static text is crisp and a moved chunk reuses every raster. Scroll
  content and transformed subtrees snap at rest: a new record or a
  changed local matrix is motion and stops snapping, so scrolling and
  animation never step; a record that has not moved for `SETTLE_SECS`
  (0.1 s) snaps again, so text at a fractional offset is crisp once
  motion stops. The settle signal is the UI clock (`Ui::set_time`, set
  by the host): `Ui::next_settle` tells the host when to wake, and
  `Ui::settle` snaps the rested records (one world row each). Scroll
  anchoring (step 2) uses the same signal. Glyph subpixel buckets stay
  relative to the chunk origin in all cases.
- Chunks build on demand within half a viewport of the screen; farther
  chunks wait and build in the frame they come into range (2026-09-23).
  Cold paint then scales with what is near the screen, as it did with
  emit-time culling.
- The draw list keys on the world and clip revisions, not on who
  derived them (a stale list was the first bug the harness found).
- Correct painter order always wins over draw-call count.

## 9. Vectors and SVG

**Current.** None. Custom content is a `CUSTOM` node with a tag, four
f32s, a text payload, and a registered painter that emits quads.

**Target.** Paths, fills, strokes, joins, caps, gradients, affine
transforms, clips, group opacity. Two preparation strategies stay
possible behind `PathRecord`: tessellation and coverage/strip
rendering. SVG is an input format only: parse, normalize, emit Craie
paths, paints, clips, images, groups. Initial SVG profile: paths,
fills, strokes, gradients, transforms, clips, images, group opacity.

**Experiments.**
- E07: lyon tessellation versus strip/coverage preparation on
  self-intersections, winding rules, thin strokes, extreme zoom, nested
  clips. Measure fidelity, preparation cost, GPU work, memory, WASM
  size.

**Decisions.**
- lyon tessellation ships first, behind the `PathRecord` boundary.
- SVG imports at build time only, via usvg, into prepared path assets.
  usvg stays out of the shipped binary. Runtime import waits for a
  product feature.

## 10. Glyph atlas and resources

**Current.** `RasterAtlas` in craie-scene: stable `RasterId`s, a
residency table the GPU reads, etagere-packed 2048x2048 R8 and RGBA
pages with CPU mirrors and dirty-rect uploads. Rasters are stamped as
chunks use them; `prepare` stamps every raster of every visible chunk
and re-rasterizes the missing ones before drawing. Eviction takes the
least recently used unstamped raster (a linear scan); when everything
is stamped, pages grow past the cap (`over_budget_pages`). A residency
row has a bitmap size and a quad size; they differ only for a glyph
rasterized smaller to fit a page (`downscaled`). `insert` still
refuses and counts a raster that fits no page (`oversized`), as a
guard.

**Target.** Stable `RasterId` with separate residency (atlas, rect,
generation). Drawing records reference the id, never baked atlas
coordinates. Before rendering: collect resource dependencies of visible
chunks, pin them, evict only non-needed resources, then prepare the
submission. A visible glyph never disappears mid-frame. Track logical
bytes, allocated capacity, CPU prepared bytes, GPU resident bytes, and
in-flight bytes separately. Budgets are runtime configuration.

**Experiments.**
- CLOCK-style eviction versus LRU. CLOCK keeps one bit in the entry;
  LRU keeps a second ordering structure, which the state principle
  disfavors. Measure under tiny budgets and over-budget visible sets.
- E09: CPU patching versus GPU residency tables for relocation.

**Decisions.**
- Add visible working-set pinning during the scene rework. Keep LRU
  until a benchmark shows its cost.
- Pin at use and before drawing, not the previous frame's visible set
  (2026-09-23). Pinning last frame's set during a scroll grew the atlas
  past its budget instead of evicting glyphs leaving the screen;
  `prepare` already guarantees every visible raster is resident.

## 11. GPU renderer

**Current.** wgpu 30. Storage-buffer mirrors of the scene pools and
tables (rects, glyphs, paints, placements, worlds, clips, raster
residency) take dirty-range uploads; an unchanged frame uploads zero
bytes. One pipeline draws both kinds (instance-index bit 31 selects the
pool) with one draw per merged run. Opacity layers render into pooled
offscreen targets and composite with their opacity. Clips test in
their own space in the fragment stage. Colors decode from sRGB to
linear in the shader; blending happens in linear.

**Target.** wgpu stays the native GPU abstraction. Craie owns pipeline
layouts, shaders, buffer layout, upload policy, and pass construction.
Uploads write only dirty ranges from retained staging. Buffer pools
follow submission completion. The renderer targets a supplied device
and render target so a host can embed it.

**Experiments.**
- E11: wgpu-on-WebGPU versus a thin direct JS WebGPU submitter. Deferred
  until a browser milestone.
- T12: cancel uploads when regenerated instance bytes are identical.

**Decisions.**
- wgpu is the production backend. No Metal/Vulkan/D3D rewrite without a
  concrete limitation.

## 12. Animation

**Current.** None. Animation is JS-driven per commit.

**Target.** A native transition driver on the UI thread.

- CSS-like transitions declared in style:
  `transition: { opacity: { duration, easing } }`.
- One imperative `animate` command for one-off tweens and springs to a
  target.
- Animatable: transform, opacity, paint colors, and layout properties
  (width, height, padding, gap). Layout properties relayout the
  affected subtree each frame.
- Transitions to and from `auto`: one relayout finds the end value, the
  tween runs numerically, and the final frame restores `auto`.
- The animation record is the source of truth during a tween. The
  per-node layout row or spatial record is the resolved input,
  overwritten each frame by one writer.
- Active animations request a redraw each frame. Idle means no work.

**Experiments.** None yet. Cost assertions: a transform or opacity
tween performs zero layouts and zero shapes.

**Decisions.**
- Native driver in the next milestone, including layout properties,
  because accordion-style expansion with reflowing siblings is a Marbre
  need.
- Per-node layout rows make the write path a single entry. No override
  table, no private style copies.

## 13. Input, focus, editing

**Current.** Platform events normalize into `Event`s. `Ui::dispatch`
hit tests through border boxes, ancestor clips, and scroll offsets, then
walks the propagation path with listener-relative coordinates. Pointer
capture holds a drag on the pressed node. Tab traverses focusable nodes
in tree order. Clipboard via arboard. IME with cursor-area tracking.

**Target.** The same model over the new stores. Pointer ids and types
rather than mouse-only concepts. Hit testing stays bounds plus clip
chain plus paint order for UI; surfaces may provide exact path tests.
Native handles scrolling, capture, focus, selection, IME, and caret
movement without a React round trip.

**Experiments.** None.

**Decisions.**
- Listener-relative coordinates stay.
- No BVH or R-tree for ordinary UI.

## 14. Accessibility

**Current.** `a11y.rs` projects an AccessKit tree from retained state.
Roles come from the explicit role field; the facade sets defaults
(Pressable, TextInput, ScrollView, Text, List, list rows) and a plain
View has none. A list row reports its position among all items and the
item count; rows appear in item order, and rows layout hides are not
published.
Bounds are transform-aware. The whole tree still republishes on any
a11y-observable change; the semantic dirty queue exists but does not
drive incremental updates yet. Actions queue back onto the UI thread.
The adapter is upstream `accesskit_winit` 0.34.

**Target.** No separate semantic store. The AccessKit `TreeUpdate` is
projected from the host node, the label table, and the layout store,
driven by a semantic dirty queue, so updates are incremental. Roles are
explicit: the host stores a role field; `Pressable`, `TextInput`, and
`ScrollView` send theirs; a plain `View` sends none. AccessKit keeps
its own internal copy because the platform requires it.

**Experiments.**
- An owned semantic tree, only if Craie moves away from AccessKit or
  projection cost appears in a benchmark.

**Decisions.**
- Explicit roles on the wire, defaults set by the facade. Native never
  infers a role from a listener.
- No SemanticNode store. It would be a second copy of role, name, and
  bounds.

## 15. Platform and window

**Current.** winit 0.30.13 confined to `craie-platform-winit`,
`ControlFlow::Wait`, frames only on `RedrawRequested`. The wake is a
unit user event. `craie_ui::platform` holds the contract: `WindowId`
and `PlatformWindow` (surface size, scale, frame request, text input);
the clipboard seam is `craie_ui::clipboard::Clipboard`. One window.

**Target.** A small platform contract: frame request, surface size,
display scale, cursor, clipboard, text input, timers, asset services,
font discovery, image decoding, events. `craie-platform-winit` is the
desktop implementation. The contract carries a window id from the
start. The core never depends on winit and never assumes it owns the
event loop, the window, the device, or the render target.

**Experiments.** None.

**Decisions.**
- Pin back to stable winit 0.30 and upstream `accesskit_winit`. The
  beta only gave us the payload-less wake and a vendored fork.
- Single window in the next milestone, with the window id in the
  contract so events and roots are keyed.
- Images decode in the platform adapter with the `image` crate behind a
  feature flag. The core receives prepared pixels and owns `ImageId`,
  dimensions, format, and residency.

## 16. Bridge and JS runtime

**Current.** As targeted. One process; React runs in a Node
`worker_thread` and submits CRW2 bytes through `NativeClient.submit`.
One threadsafe function delivers `ack | events` frames. JS recycles
ids at once and mirrors each slot's generation; events carry the
generation and JS drops stale ones; the ack only resolves `flush()`.
Payload ops copy typed-array bytes once. Protocol version 2.

**Target.** The same transport with CRW2 payloads. The bridge exposes
`submit` and `subscribe` only, so an embedded JS engine could replace
Node later without touching the wire. Payload ops carry typed-array
bytes for surfaces, copied once, atomic with the commit.

**Experiments.** None. The wire path is measured as free.

**Decisions.**
- Keep Node plus N-API. The bridge stays transport-agnostic.
- Bulk surface data crosses as a copied typed-array op in the same
  transaction. No shared-memory ring.

## 17. Scheduling

**Current.** Everything runs synchronously on the UI thread inside the
frame: apply, layout (only when inputs changed), chunk updates near the
viewport, residency, upload of dirty ranges, submit. Cost counters
exist (`craie_core::counters`).

**Target.** Single-owner, demand-driven preparation with one frame
path: apply mutations, run text and layout work, update spatial state,
update chunks, resolve residency, upload dirty ranges, encode passes,
submit. No second quick-redraw path. Counters and revision stamps exist
now so a job pool with generation-stamped publication can attach later
without changing publication rules.

**Experiments.**
- Job pool for shaping and raster with atlas epochs, once a workload
  shows the UI thread is the bottleneck.

**Decisions.**
- Synchronous in the next milestone. Add counters and revisions first.

## 18. Verification harness

**Current.** `harness/invariants` holds: incremental equals clean
rebuild over seeded mutation sequences, on the pinned fonts (layout, drawn scene, hit
tests, semantics; 8 seeds x 60 steps at 1x and 2x with a moving
clock, compared at rest, plus resize and
atlas-pressure cases); the cost invariants that apply today (color
change, width change without shaping, translation, scroll, unchanged frame, tween, atlas
relocation, input color, typing shapes with composition controls and
a layout-identity oracle); allocation tests over the whole frame on a
real device with a separate budget per phase. Craie's phases (UI
render, `Renderer::collect`, `plan_frame`) allocate nothing on an
unchanged frame, a warm color or transform patch, or patch and idle
frames in alternation (dirty ranges merge in place). wgpu's phases are
measured apart: `upload` costs wgpu 8 allocations per buffer write,
and `encode_frame` a fixed 56 per frame plus 23 per extra pass on
Metal (5 more after a write), the same every frame; a copied-bytes counter
that includes text and span lists; real-GPU checks (upload bytes, 1x
and 2x pixel readback, half-pixel edges against the resolver, a glyph
larger than a page); list tests (only the reported range renders; a
virtualized list equals a plain column of every row once measured;
padded, bordered, gapped (fixed and percentage), definite-height, and
max-width lists with a following sibling, checked against an f64
reference of the same f32 terms within bounds derived from each path's
additions (the list: γ₄; the plain column: γ₃ᵢ₊₃; the list under 1/8
device px at 2x); percentage gaps under min/max on a root list; size
probes against final layout on wide corrections; keep-visible under measurement, inserts above, and reorders
across the viewport, with a no-anchor control; stick-to-end; range
hysteresis; transforms on the list and on an ancestor, checked by hit
testing; a flipped list; identity uniqueness through the batch and an
identity-index oracle after every seeded step; strict item flags;
edits against moves for measurement retention; a fallback change
against a rebuild; layout visits per
rendered row; a focused row stays and survives splices above; seeded
splices, edits, and scrolls equal a clean rebuild; list frames allocate
nothing in the UI (always) and in Craie's renderer phases (on a GPU); accessibility
positions and hidden rows);
the release-graph
and layer-map checks; E01 (the owned paragraph against Parley on the
pinned fonts: line breaks, cluster maps, and drawn glyph ids exact,
advances bit-equal, positions and line metrics within a bound derived
from the f32 additions of each value, L1 lines checked by moving
Parley's trailing whitespace, and negative controls for each kind of
difference; unicode-bidi's own L1 + L2 as the reference for line
order; rewrap equals a fresh layout from every start width; drawn
glyphs compared by id, source cluster, and font file; byte-to-cluster
queries at every byte; breaks, emoji, fallback, and missing-glyph
cases; each known difference asserted exactly: CRLF and NEL against
UAX #14, no-break spaces, and one font per grapheme); owned-text unit
tests (carets, selection, and hits equal to the placements on
multi-glyph clusters and L1-reversed segments; line starts at UAX #14
opportunities; fallback that depends on the cluster alone; byte
identity across a replaced source); a width change does no shapes;
release builds without test features (`pinned-fonts`), with the
harness and a workspace build as negative controls; and the E01, E10,
and E14 benches (`examples/fontcost` splits a fresh engine's font
work). `prepare_frame` in
platform-winit is the one frame path for commits and native input;
accessibility bounds and the IME area publish only after it.
The host sets the UI clock and wakes at `Ui::next_settle` to snap
rested content. `CRAIE_CAPTURE=<png>` makes the host write one settled
frame and exit
(used to check the JS examples' layouts). Crate tests cover the span pool,
scene, host, wire, executor, dispatch, editing, and a11y. `bun test`
covers the encoder and the reconciler. Benchmarks: `examples/bench`,
`examples/framebench`.

**Target.** `harness/` crates with:

- oracles: Taffy for layout, Parley and HarfBuzz for text, CPU
  reference renders for vectors, browser cases for layout;
- the central invariant: incremental update equals clean rebuild, over
  generated mutation sequences, for layout, placements, selection, hit
  tests, semantics, retained drawing, and pixels within tolerance;
- cost invariants: color change does zero shapes and zero layouts;
  translation does zero layouts; unchanged frame regenerates zero
  chunks and uploads zero bytes; atlas relocation does zero paragraph
  layouts; a tween on transform or opacity does zero layouts;
- fuzzers and model-based tests for handles, pools, and residency;
- the benchmark matrix from the vision page, with pinned viewport,
  scale, fonts, hardware, profile, corpus, and seed.

**Experiments.** The harness is what runs them.

**Decisions.**
- In-repo workspace crates. Reference libraries are dev-dependencies
  only. CI asserts release graphs are clean.

## 19. WASM and browser

**Current.** No WASM build; `cargo check --target
wasm32-unknown-unknown` passes for core, scene, render, and text.

**Target.** Build profiles from `core + scene + render` for a tiny
surface up to `+ ui + react` for the full runtime. Browser text uses
capability-specific profiles: portable WASM typography, browser-rendered
labels, or DOM text. Size tracked per profile: raw, wasm-opt, gzip,
brotli, JS glue, embedded tables.

**Experiments.**
- E11: browser submission boundary.
- E12: browser text profiles.

**Decisions.**
- Desktop drives the next milestone. `cargo check` for wasm32 on core,
  scene, render, and text keeps the boundaries honest. No shipped WASM
  build yet.

---

## Order of work

1. Crate split with CRW2, chunk records, the span pool, and the winit
   downgrade.
2. Native list virtualization inside ScrollView, with anchoring.
3. Owned text: paragraph, editing, selection, over HarfRust and skrifa.
4. Native animation driver.
5. lyon paths and build-time SVG assets.
6. Owned flex layout behind the differential harness. Block and grid
   follow.

## Non-goals for this milestone

Inline formatting context, runtime SVG import, multiple windows, a
shipped WASM build, a text job pool, coverage-based vector rendering,
controlled inputs, native mobile adapters, a second reconciler in Rust.

## Open configuration

Numeric budgets for atlas pages and the layer pool are runtime
configuration. Defaults get recorded here when the first benchmark under
pressure runs.
