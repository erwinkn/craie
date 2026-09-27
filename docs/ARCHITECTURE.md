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
  editor.rs             owned editing: cursors, selection, IME preedit
  fonts.rs              FontStore, FontSource, RawFonts, pinned fonts
  lib.rs cache.rs       engine, emit into chunks, GlyphKey -> RasterId
crates/ui/              craie-ui: core, scene, text, Taffy, accesskit
  mutation.rs           Mutation enum, Transaction + direct-API builder
  wire.rs               CRW2 decode/encode
  executor.rs           validate + apply: revisions, dirty queues
  host.rs               16-byte headers, per-node stores, child spans
  layout.rs             Taffy over host rows; moved/resized tracking
  scene_sync.rs         host + layout -> chunks, placements, records
  dispatch.rs           hit testing, focus, pointer capture, editing
  reach.rs              hit-test index: reach box per subtree (E15)
  ui.rs                 facade; a11y.rs, input.rs, surface.rs, events.rs
  bridge.rs platform.rs Session (commit queue, outbox), platform contract
crates/platform-winit/  winit 0.30 driver, Window, HostApp, clipboard,
                        fonts.rs (SystemFonts: fontique behind FontSource)
crates/node/            N-API: NativeHost, NativeClient
harness/invariants/     craie-harness: invariant, cost, graph tests; E01
                        (Parley oracle), E10, E14, E15
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

**Current.** CRW2 as targeted. The animation family (0xA0, step 4):
a transition op replaces a node's declared transitions (count u8, then
per property a u8 and a 25-byte timing), and an animate op tweens one
property to a target (the target in its property's shape: transform 6
f32, opacity f32, a color u32, width or height f32, padding 4 f32, gap
2 f32; lengths only). A timing is a kind u8 (curve or spring), a delay,
and five f32 (a curve's duration and control points, or a spring's
stiffness, damping, and mass). Validation rejects unknown properties
and timing kinds, a property declared twice, non-finite or
out-of-range timings (curve x control points outside [0, 1], an
undamped spring, more than 600 s), targets of the wrong kind, and paint
animations on nodes without a box.
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
positional fields), and text spans (28 bytes each since wire version 3,
step 3c: start, size, color, weight, flags for italic, underline, and
line-through, a family string reference or NIL, letter spacing, and
line height; span zero is the base, and a paragraph has no family of
its own). The interaction op's flag byte carries focusable,
selectable, inert and auto-focus (bits 0 to 3, the last two since
protocol 8), then pressable, disabled and keep-focus (bits 4 to 6,
protocol 7); unknown bits fail decoding. The trap op (0x62, protocol
8) sets a node's focus-trap flags: active, modal, auto-focus and
restore-focus (ARCHITECTURE-update topic 3). The group op (0x63,
protocol 10) makes a node a focus group: horizontal, vertical, loop and
select-on-focus, no bits unmaking it. The claims op (0x61, protocol
4) replaces a node's claim set, or the window list's (NIL, key claims
only): a version u32, a count u16, then per claim a kind, flags,
modifiers, a pad byte, and a key u32 (a named key's code or a
character). Decoding rejects unknown kinds and flags, modifiers past
the four, and keys that name nothing. The input config's flag byte
carries multiline (bit 0) and the submit key (bits 1 and 2). Commands
add `InsertText` (a paste claim's answer) and `WriteClipboard` (copy
and cut; NIL may send it). The spatial op's mask carries z (bit 2, an
i32; work item 4), and the layer op (0x22: id, then the owner or NIL)
makes a node a layer container. The state family (0xB0, protocol 4, work
item 5): `STATES` sets a scope's app bits (u64; the input bits are
native's, and setting one fails validation); `VARIANTS` replaces a
node's variant table (per variant: terms of a scope id and a u64 mask,
environment bits, then values under a u8 mask: fill, border color,
radius, color, opacity, transform, layout, border width; layout
is a u64 of keys, one per property, axis and side, then the style
fields that hold them, so `_narrow: { padding: { left: 4 } }` and
`_compact: { padding: { top: 6 } }` both apply), and a count of 0
removes it (at most 256 variants of 8 terms); `ENVIRONMENT` sets the narrow
and compact breakpoints; `COLOR` sets or clears the color a node's
text, inputs and `currentColor` drawings inherit (protocol 5: the input
config carries no color, and a drawing shape a `current` flag byte).
Span flag bit 3 marks a span that inherits its color.
Validation rejects dead nodes and scopes, unknown bits and layout
keys, box values on text, oversized tables, and non-finite breakpoints. Ops are u8-tagged,
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
layout inputs), spatial (transform, opacity, z), paint (fill, border,
radius, colors), text (paragraph spans), semantics (role, label),
interaction (listener mask, focusable), claims, list (item count, estimates,
item descriptions), animation (transitions, animate command), payload
(typed-array bytes for surfaces), command (focus, blur, setText,
scrollTo, insertText, writeClipboard).

**Experiments.**
- Per-transaction style intern table versus inline styles on every
  set_style. Measure wire bytes, encode time, decode time on the mount
  and stream workloads.

**Decisions.**
- One breaking redesign during the split. Golden fixtures regenerate
  once.
- Transform, opacity and z travel in their own spatial op and never
  touch layout inputs. The facade splits the style object at commit
  time (`zIndex` goes with transform and opacity).
- Styles are stored per node natively. Interning, if kept, is transport
  compression only and dies with the transaction.
- The `hidden` op is removed. `display: none` is the one way to hide.
- Synchronous JS painters are removed. Custom content is native
  components and surface records fed by payload ops.
- The ack-gated id free list is removed. JS recycles ids immediately.
  Outbound events carry the node generation and JS drops stale ones.
  The ack remains only to resolve `flush()` and, since protocol 4, to
  retire the handlers of replaced claim sets.
- Span lists intern per transaction like styles (2026-09-23): most
  paragraphs share one style, and without interning CRW2 cost 9% more
  bytes than CRW1 on the 5k-row mount (see `EXPERIMENTS.md`).

## 3. Host tree and storage

**Current.** As targeted. `NodeHeader` is 16 bytes: parent, child
span, kind, flags, generation. Dense per-node stores: `layout`
(`taffy::Style`, 240 bytes), `spatial` (transform, opacity, z, scroll
offset), `paint`, `paragraphs` (UTF-8 + span list), `interaction`
(listeners, focusable, role). Labels and surface payloads are id-keyed
maps, and so are paint orders and layer owners (`order.rs`, work item
4): a parent whose children have a z or a layer keeps them sorted in
`orders` (header flag `SORTED`; `ORDER` while queued for re-sorting),
and every other parent paints in tree order at no cost. The nine revisions live on the host; dirty queues (layout,
content, paint, spatial, semantic) are `DirtyQueue`s.
State styles (`states.rs`, work item 5) keep id-keyed stores of their
own: scopes (app bits, input bits, the ids of the tables that read
them) and variant tables (the base, which the node's own ops set; the
variants, sorted by specificity; the values last resolved), and a
queue of tables to resolve again. Inherited colors are an
id-keyed map (`Host::colors`).

**Target.** The same arena shape with per-usage stores:

```
nodes[]            header: parent, kind, generation, flags, child span
layout_inputs[]    dense per-node layout row (no shared records)
spatial[]          local transform, opacity, z, clip ref
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
- The compact row came first, ahead of the owned engine (2026-09-24,
  step 6a): Taffy reads it through its style traits, so the memory win
  does not wait for the engine, and the engine (step 6b) is written
  against the row it will read.

## 4. Layout

**Current.** Craie's own flex engine (`craie-layout`, steps 6b and
6c), reading each node's own layout row: a Craie-owned `LayoutRow` (136
bytes, step 6a) that holds every `taffy::Style` field the flexbox
build reads (lengths in one tagged `f32` array; grow, shrink, aspect
ratio, and scrollbar width as `f32`; enums, alignment safety,
direction, and containment as bytes and flags), so no `taffy::Style`
is stored per node (the row also implements Taffy's style traits, for
the harness's Taffy reference); transactions
still carry taffy styles, converted when a layout op applies. The row
drops only `item_is_table` and `item_is_replaced`, which only the
block and grid algorithms read. Results land in `Layouts` (cache, unrounded
layout, final rect, content-box offset); finalize records nodes whose
origin moved, whose size changed, and whose scroll extent changed. A
layout pass runs only when the layout queue is non-empty or the
viewport changed, so an unchanged frame does zero layouts. React
Native defaults hold natively: a node with no style, a partial style,
or a reset style is a flex column whose children do not shrink
(`flexShrink: 0`; `host::default_style`). Cold layout
of 10k nodes costs 217 to 567 ms; one streaming append costs 0.65 ms.

The engine: `compute_flex`, `compute_leaf`, `compute_hidden`,
`compute_root`, and `compute_cached` over a `LayoutTree` that
`layout::TreeView` implements (rows, children, its dispatch to hidden,
list, flex, or leaf layout, results, the per-node cache with hit and
miss counters, and reused item and line buffers up to 256 items per
level, taken best fit). Laying out again containers it laid out before
allocates nothing when they have at most 256 children (tested after
content, style, and viewport changes); a container new to the pool, or
a wider one, allocates. It is a port of Taffy 0.14's flex algorithm that reads
`LayoutRow`s directly, and it equals Taffy bit for bit on the E05
differential suite. Taffy stays a dependency for its value types and
cache and as the harness reference; no Taffy algorithm runs in
production. Against Taffy in the same host (step 6c, `examples/bench`,
interleaved runs under load): layout times equal within noise, layout
allocations halved (cold 10k rows 90,203 to 50,207; 500 dirty rows
8,028 to 4,027), live heap unchanged.

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
- E05: the owned engine, then specialized kernels, versus Taffy on
  generated trees with percentages, baselines, min/max, intrinsic
  sizes, overflow. Step 6b: the general flex engine is bit-equal on
  2,400 generated trees (cold and after six edits each: styles,
  content, moves, viewports), on 28,800 direct calls with every kind of
  `LayoutInput` (full `LayoutOutput`s compared), and on the
  harness's `Gen` trees; step 6c made it the production path; see
  EXPERIMENTS.md.

**Decisions.**
- Virtualize first. The measured cold cost comes from visiting every
  node, and a virtualized list removes it without replacing Taffy.
- Own flex, then block, then grid. Taffy stays the production general
  path for each algorithm until the owned one passes its differential
  suite against Taffy in the harness. Taffy may ship for grid for a
  long time.
- The owned flex engine starts as a port of Taffy's algorithm (the
  same steps and float operations in the same order), not a new
  implementation. Reason: the gate is equality with Taffy, and a port
  can be bit-equal, so the suite needs no tolerance and any difference
  is a defect. Kernels come later, behind the same suite. (Step 6b.)
- The owned engine keeps Taffy's value types (`Size`, `Rect`,
  `AvailableSpace`, `LayoutInput`, `LayoutOutput`, `Layout`) and
  Taffy's `Cache`. Reason: the cache decides which earlier results a
  layout reuses, so another cache gives different results; the value
  types are plain data that `Layouts` already stores. The algorithm,
  the tree interface, and the buffers are Craie's. (Step 6b.)
- The host owns invalidation: the engine never clears a cache except
  in hidden layout, as in Taffy. (Step 6b.)
- The engine's buffer pool keeps buffers of at most 256 items. Reason:
  kept buffers hold their peak size (248 bytes per item), and one
  5,000-child column added 2 MB of live heap at 5k rows; wider
  containers allocate per call, as Taffy does. (Step 6c.)
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
default family, `system-ui` (Decisions). A span carries the style
subset React Native allows on nested Text (family, size, weight,
style, color, underline and line-through, letter spacing, and line
height, which is span zero's for the paragraph); the executor resolves
each span's family to a `FontInstanceId` once, when the paragraph is
applied, and per-cluster fallback stays the one fallback path.
Decorations draw as rects from the placements in the span's paint slot.
INPUT nodes hold an owned
`Editor` each (`craie-text/src/editor.rs`, step 3b): one UTF-8 buffer
with the IME preedit inside it (`raw_text`; `text` excludes it), a
selection of two cursors (a byte index at a cluster boundary and an
affinity, so a soft line break has a caret at each line), and the
layout as an owned paragraph. Left and right move one cluster in
visual order; up and down keep a horizontal goal; word moves and
deletions use UAX #29 word boundaries; Backspace removes one code
point after a combining mark and a whole newline or emoji cluster.
Carets stop only at grapheme boundaries. Every operation of Parley's
`PlainEditor` that the input used is kept, with its behavior (E01
editable cases). An edit shapes once through the engine; a width
change only rewraps; caret motions allocate nothing. Caret, selection,
and IME-area geometry read the placements. An edit that overlaps the
preedit ends composing; `set_text` moves the selection onto character
boundaries at once. Undo snapshots keep full cursors; a composition is
one undo step from its start to its commit. `TextInput` is uncontrolled
(`value` is sent once at mount; `setText` replaces the text without an
`onChangeText` echo); `onChangeText` fires only when the committed text
changes, including when a composition finishes (ImeDone, focus loss).
An edit records an undo entry only when it changes the text. Parley is
not a dependency of any release crate: the harness keeps it as the
oracle. Validation bounds font sizes to `MAX_FONT_SIZE`
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
  (Current since step 3c: a Text inside a Text is virtual in the bridge
  host; the outermost Text composes its paragraph and listener mask at
  each commit's seal, each span inheriting its parent's unset style.)

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
- Letter spacing follows CSS Text, not Parley (2026-09-23, step 4
  opening). CSS Text Module Level 3, `letter-spacing`
  (https://www.w3.org/TR/css-text-3/#letter-spacing-property): when the
  effective spacing between two characters is not zero, user agents
  should not apply optional ligatures, but must still apply required
  ligatures. A run whose letter spacing is not zero shapes with `liga`,
  `clig`, and `dlig` off; `rlig`, `calt`, and mark positioning stay on.
  Each grapheme then takes the spacing once. The web and Android do the
  same, and the kit targets the React Native text vocabulary. Parley
  keeps the ligatures: a known difference with its own exact test
  (`spaced_runs_drop_optional_ligatures`).

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
alone. fontique is reachable only through the platform adapter: the
layer map test asserts that craie-text and craie-ui reach neither
fontique nor Parley. The rasterizer sets a face's variation
coordinates on every scaler, empty included (swash keeps them across
builders).

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

**Current.** As targeted, except `ImageInstance` and group opacity by
multiply-through (only isolated layers exist). An image draws as a
color glyph: one `GlyphInstance` quad of its node's raster, sized to
the fitted rect, its origin on a device pixel (work item 8). One chunk per node (id =
node id) in `RectInstance` (40 B) and `GlyphInstance` (20 B) pools, a
paint pool, and path mesh pools (`PathVertex`, 16 B: chunk-local
position, paint, chunk; and triangle indices), with up to four
same-kind segments in paint order (step 5a). A gradient paint is a run
of words in the paint pool (kind and stop count, geometry, the
chunk-local to gradient affine, then offset and color per stop), so
meshes need no second paint table. A placement table (offset, transform record, clip) positions
each chunk. Transform records exist for the window root, scroll
content, and transformed subtrees; all other nodes draw in their
nearest record's space at an offset. The draw order, layer table,
record evaluation order, and clip records are rebuilt by one walk when
structure or clip topology changes; layout moves revisit only moved
subtrees. The walk visits each parent's children in paint order: tree
order stably sorted by z, as React Native's `zIndex`, with no stacking
contexts (work item 4). A z change counts as a structure change. Chunks build on demand within half a viewport of the screen.
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
  not incrementally patched state. The measured walk is cheap. A z
  change bumps `structure_rev` too: it reorders without moving
  anything, so it rebuilds the draw order and costs no layout. Under
  it, each parent's sorted child order is a second cache, re-sorted
  only for the parents that changed (work item 4).
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

**Current.** Step 5a: `craie-vector` builds paths (lines, quadratic
and cubic Béziers, subpaths) and tessellates fills (nonzero, even-odd)
and strokes (miter, round, bevel joins with a miter limit; butt, round,
square caps) with lyon into meshes at a caller-chosen tolerance; paints
are solid colors and linear and radial gradients (pad spread). The
scene draws meshes and never sees lyon (layer map test). Surfaces
(step 1) remain for native painters that emit quads.

Step 5b: SVG is imported at build time. `tools/svg-import` (library
and the `craie-svg in.svg out.crv` tool) runs usvg and writes a
`CRV1` asset (`craie_vector::asset`: view box, paints, and items of
path, fill rule or stroke, paint, opacity, and transform), reporting
every feature it cannot represent (the tool fails on one unless
`--lenient`). A `Vector` node (kind 5) takes the asset as its payload;
validation decodes it (a bad asset rejects the transaction). Its view
box is its intrinsic content size (a set dimension, and min and max
sizes, scale the other axis); the drawing fits the content box,
centered, aspect kept, and is clipped to its view box as an embedded
SVG is. Each item
tessellates in its own space at a quarter device pixel of tolerance,
maps into the chunk, and is cached per node until the content box, the
display scale, or the asset changes. JS: `<Vector asset={bytes} />`,
an image for assistive technology by default.

Work item 8: shapes also arrive at runtime as SVG strings. A `DRAWING`
op (0x72) carries a view box and shapes: path data or points, a
transform list, a dash array, and resolved paint (plain colors, fill
rule, stroke width, joins, caps, dash offset, opacity).
`craie_vector::svg` parses them into the same asset a `CRV1` payload
decodes to. Work is bounded per drawing: at most 4,096 shapes whose
string references add up to at most 4 MiB, checked first; each
distinct string parses once, against one budget of 2^20 path commands,
transform functions and points; 64 numbers per dash array and 65,536
dashes per drawing. Past the shape or byte bound the transaction is
rejected; a value that does not parse draws nothing (the node has no
drawing and no intrinsic size) and the session goes on. Strokes take
dashes (`craie_vector::dashed`: the flattened path cut by length,
restarting on each subpath, zero-length dashes as dots, a closed
subpath joined through its start). Sources, payload bytes (starting
`CRV1`) or a drawing's canonical key (starting `CRVS`, built once per
transaction), are interned by content, and meshes are cached per
asset, content box and display scale, so 200 nodes showing one icon
parse once and tessellate once. Tessellation skips shapes outside the
view box or at opacity 0 and flattens no finer than a shape's size
over 2^16. The facade resolves `currentColor` to a `G`'s `color`, or
leaves it to native, which paints the node's inherited color (§13)
through its own paint slot, so a hover recolors without tessellating;
`Vector`'s `opacity` is the node's opacity (one layer, animatable);
a `G`'s multiplies into its shapes. JS:

```tsx
<Vector viewBox="0 0 24 24" fill="none" stroke="#fff" strokeWidth={2}>
  <Circle cx={12} cy={12} r={10} strokeDasharray="4 2" />
  <Path d="m9 12 2 2 4-4" />
</Vector>
```

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
- SVG documents import at build time only, via usvg, into prepared path
  assets; path data is runtime input (2026-09-27, work item 8). usvg
  stays out of the shipped binary: the runtime parser reads path data,
  points, transform lists, view boxes and dash arrays, not documents,
  styles or text.
- Assets keep paths, not meshes (2026-09-23, step 5b): tessellation runs
  at the display scale on the device, so a drawing is as smooth at 16
  px as at 512 and one asset serves every size. Group opacity folds into
  each item's paint (exact when a group's children do not overlap; the
  importer reports the other case). Unsupported SVG features are
  reported, never silently dropped (`LEDGER.md` DF-6).

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
Image nodes own color rasters (work item 8): a raster per decoded
bitmap, at the decode size, its quad set to the drawn size each chunk
build (`set_quad`), so a bitmap draws scaled until a better one lands.
The core keeps each bitmap's pixels, up to 64 MB for all images (least
recently drawn dropped first; never one it could not decode again: an
old `src` up while the new one loads, or a payload that failed since), and re-inserts an evicted one when a
visible chunk misses it, as text re-rasterizes glyphs; one whose copy
was dropped decodes again. `release` frees a raster's area and
recycles its id when the node's image changes or goes; glyph rasters
are never released. A color raster's 1 px gutter repeats its edge
pixels (a mask's is empty), so a magnified bitmap keeps its full color
to the edge under linear filtering. Rasters stay straight alpha, as
the shader expects: the decoder averages premultiplied and gives
transparent pixels their neighbours' color, so filtering shows no dark
rim. Large images share the atlas's pages (DF-36).

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
residency, path vertices and indices) take dirty-range uploads; an
unchanged frame uploads zero bytes. One pipeline draws rects and
glyphs (instance-index bit 31 selects the pool) with one draw per
merged run; a path pipeline pulls mesh triangles through the index
pool (vertex index -> index -> vertex) from the same tables. A frame
run of consecutive mesh draws renders into a 4x multisampled layer of
its own, resolved and composited in painter order; the window and
opacity layers stay single-sampled. Opacity layers render into pooled
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
- Paths anti-alias by 4x MSAA in layers of their own (2026-09-23, step
  5a; revised after review rounds 1 to 3). Every maximal run of
  consecutive mesh draws becomes a multisampled layer (opacity 1, bounds
  of the run's chunks), resolved and composited in painter order;
  compositing a run "over" its parent equals drawing it in place, since
  "over" is associative. The window and opacity layers stay
  single-sampled, so rects and glyphs keep their exact quads and their
  own analytic coverage. The first design (the whole frame at 4x when it
  drew any mesh) failed three review rounds: analytic coverage and
  sample coverage multiplied at every edge (rects, glyphs, clips), each
  padding fix brought new edge cases (zero-area rects, anisotropic
  transforms, enlarged and reduced glyphs, layer bounds), and the first
  multisampled frame after single-sampled ones sometimes came out
  unwritten (about 1 in 20 runs; none in 160 since). Inside a path
  layer, fragments shade per sample and test each clip of the chain at
  the sample's position (the standard 4-sample pattern), so path and
  clip coverage never multiply. Content between meshes that overlaps
  none of the open run draws before the run (it only has to stay below
  the run's later meshes), so the run goes on; content that overlaps
  the run ends it, and meshes on the two sides of it anti-alias apart
  (a shared edge then shows a faint seam, as it does between separately
  drawn shapes in browsers). Cost: one offscreen pass and one composite
  per run (`LEDGER.md` AR-5).

## 12. Animation

**Current.** The native driver (`animation.rs`, step 4). A node's
declared transitions live on the host (`Host::transitions`, id-keyed),
running animations in the driver (`Ui::animations`), at most one per
node and property, found through a (node, property) index: lookups are
O(1) however many tweens run (a ripple over 10,000 tiles starts 10,000
at once). A mutation of a property with a running animation
compares with its declared target: equal changes nothing, another value
retargets it from the value on screen (with a transition) or cancels it
and jumps (without one). Without a running one, a declared transition
starts a tween from the value on screen. `render` first advances every
animation to the UI clock and writes the rows through the mutation's
own writers (`set_layout`, `set_spatial`, `set_paint`); the final frame
writes the declared value. A size target that is not a length is
resolved by one probe: a layout compute with every running layout
animation at its declared value, without anchoring, scroll commands,
or offset clamps, and with list rows laid out but not recorded (no
measurements, no estimates), after which the rows go back and the
frame lays out in full, lists included. A padding or gap that is not a length jumps (`LEDGER.md`
DF-4). A size starts from the laid-out size only when the slot's
current occupant has been laid out (`Layouts::is_laid_out`); a retarget
starts from the sampled value clamped as the row writer clamps it. Transforms interpolate as rotation x
upper-triangular x translation; colors premultiplied. Curves are CSS
cubic-bezier; springs are damped oscillators from rest. `needs_paint`
holds while anything runs, and the platform asks for the next frame
after each one. JS: `style.transition` (per property: a duration,
delay, and easing, or a spring; milliseconds) and `node.animate(prop,
to, timing)`, which resolves with how the tween ended: an
`ANIMATION_END` event (node, generation, property, and reason:
finished, cancelled, retargeted, or removed) on the event channel,
routed per property in call order; releasing a node resolves its
pending calls as removed. State variants feed the same path (work item
5): a restyle declares only the fields that differ from the values last
resolved, through the mutation's own interception, so a declared
transition tweens a hover fill. A table's first resolution, and the
first frame's environment, write directly (a row mounted selected
does not fade in), and cancel an animation on the same property. The
inherited color (prop 8) tweens between two set colors; from none
it jumps.

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

**Experiments.** Cost assertions: a transform or opacity tween performs
zero layouts and zero shapes (asserted, `EXPERIMENTS.md` Step 4).

**Decisions.**
- Native driver in the next milestone, including layout properties,
  because accordion-style expansion with reflowing siblings is a Marbre
  need.
- Per-node layout rows make the write path a single entry. No override
  table, no private style copies.
- Transitions follow CSS where it decides (2026-09-23, step 4): a
  transition declared in the same commit as a change applies to it
  (the after-change style), first values at mount do not tween, and a
  changed transition set affects later changes only. `animate` leaves
  its target in the row until a commit sets that property again (as
  React Native's native driver).

## 13. Input, focus, editing

**Current.** Platform events normalize into `Event`s. `Ui::dispatch`
hit tests through border boxes, ancestor clips, and scroll offsets,
children in reverse paint order (z included; a layer container's own
box lets hits through; an inert node and, under a modal trap,
everything outside it are skipped),
skipping any subtree whose reach (a box around everything it can hit,
kept lazily and refreshed after each frame's layout; `reach.rs`, E15)
misses the point, then walks the
propagation path with listener-relative coordinates. Pointer
capture holds a drag on the pressed node. Tab traverses focusable nodes
in tree order, whatever their z, each owned layer right after its
owner's subtree, inside the innermost active focus trap (`trap.rs`); a
focus group is one stop, and arrows, Home and End move among its
members (`group.rs`). Clipboard via arboard. IME with cursor-area tracking.
A pointer event on a text node carries the span under the pointer (key
bits 16 and up, so at most 65,535 spans per paragraph), found from the
placements, and the paragraph's revision (paragraph ops applied, a
u32 in the 36-byte event record; runtime protocol 3). JS routes the
event to the nested Text that owns the span when the revision is the
one it last sent, else to the root: a nested Text replaced since does
not get an event hit-tested against the old span table (step 3c).
Read-only text selection (`selection.rs`): a `selectable` node makes
its text descendants one domain; a primary press starts a selection in
the text under the pointer, else in the text nearest it (the distance
to its transformed content box in window space); a drag moves its
focus, shift extends; Cmd/Ctrl+A selects the domain, Cmd/Ctrl+C copies
the selected text in tree order (one line per paragraph of the
interval, empty ones included), Escape or a press outside (an input
too) clears. The selection holds its nodes with their generations: a
removed or reused endpoint, a domain no longer selectable, hidden, or
detached (itself or an ancestor), or an endpoint no longer a displayed
text of the domain drops it. After every transaction and
before paint (list rows change natively), the highlighted ranges are
recomputed from the tree; each selected paragraph draws its highlight
from its placements in its own chunk, and only the chunks whose range
changed rebuild. The rebuild oracle carries the selection.
Keys and claims (`claims.rs`, protocol 4): a key record packs the
modifiers, repeat, composing, the named key, and the physical key's
US-layout character into `key`, and carries the logical character
(Shift and Alt applied, as the web's `event.key`) as text. A claim set
on a node, or the window list (`useHotkeys`), takes a gesture before
any default. A key looks at the focused node, its ancestors, then the
window list, which matches only claims that allow inputs while a text
input has focus; nothing matches while an IME composes, and a claim
with `repeat: false` swallows its auto-repeat. A chord is exact
modifiers plus a named key or the lower-cased character; with Alt and
a letter or digit, or a non-Latin letter, the physical key. Native
editing commands use the same rule with exact `mod`. Paste, copy, and
cut are claimed on the focus path (the selection's domain when nothing
has focus), drops and secondary presses on the path under the pointer
(a drop at an unknown position on the focus path; on macOS Ctrl+click
is a secondary press), and the ContextMenu key and Shift+F10 on the focus path at the focused
node's center. A match sends one `CLAIM` event (claimer, kind and
index, the set's version, and the clipboard text, the selected text,
the dropped paths, or the position) and no default; JS answers with
`InsertText` or `WriteClipboard`. An unclaimed key goes to the focus
path, and to no one when nothing has focus. Escape has no action in an
input; Enter follows the input's submit key (`enter`, `mod+enter`, or
`none`, with exact modifiers).
State bits (work item 5): after every dispatch, transaction and
accessibility action, native recomputes the input bits of the scopes
on three ancestor chains, and only when the hovered node, the press,
the focus, the modality or the tree changed: hover on the hovered
node's ancestors, pressed on the primary press's, focus-within on the
focused node's, and focus-visible when the last input was a key (not
a bare modifier or a Cmd, Ctrl or Alt chord) or focus is in a text
input. Disabled masks hover, pressed and focus-visible. A change
restyles only the tables that read the scope. Hover follows geometry
at rest: after a frame that moved geometry, native hit-tests the still
pointer again (only if a table or listener reads hover), and during a
scroll it holds until the settle signal. Leaving the window clears the
pointer; a wheel event sets it. Detaching or removing a node ends a
press inside it; focus stays on a node that moves.

**Target.** The same model over the new stores. Pointer ids and types
rather than mouse-only concepts. Hit testing stays bounds plus clip
chain plus paint order for UI; surfaces may provide exact path tests.
Native handles scrolling, capture, focus, selection, IME, and caret
movement without a React round trip.

**Experiments.** None.

**Decisions.**
- Listener-relative coordinates stay.
- No separate spatial index (R-tree, rebuilt BVH): the node tree
  carries a bounding box per subtree, which makes the hit test 12x to
  236x faster (E15). The full walk stays as its oracle.
- Hit order is the reverse of paint order, z included; Tab, selection
  and accessibility keep tree order (work item 4). A subtree's box is
  a union, so a z change leaves it as it is.

## 14. Accessibility

**Current.** `a11y.rs` projects an AccessKit tree from retained state.
Roles come from the explicit role field; the facade sets defaults
(Pressable, TextInput, ScrollView, Text, List, list rows) and a plain
View has none. States come from a scope's bits: the check roles report
`checked`, and `expanded` and `selected` appear where the facade says
the prop was given, `selected` on list rows and tabs only
(ARCHITECTURE-update §13). Under an active modal trap, AccessKit's `modal` goes on the
trap's first `dialog` or `alertdialog` node, else on the trap. A list row reports its position among all items and the item
count; rows appear in item order, and rows layout hides are not
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
unit user event. A commit is applied and asks for a frame; the frame
is prepared only in the redraw, once per presented frame (preparing
after every commit starved the redraws under a stream of commits).
A frame that moves geometry (a layout pass, an animation sample) marks
the accessibility tree stale. The redraw publishes accessibility and
the caret area right after it prepares the frame (a guard on `needs_paint`, which holds while any
animation runs, had held them back for whole animations since step 4).
Running animations request the next frame only after
a frame presented: an occluded window gets no drawable, and asking
again at once spun the loop (about 1,700 empty redraws a second). The
frame loop measures each frame's CPU time (the frame path plus draw
encoding and submission) and, about twice a second while frames are
drawn, sends a `FRAME_STATS` event (frames per second, mean and worst
CPU ms, layout and scene ms, live nodes, running tweens; droppable)
that JS reads with `useFrameStats` or `onFrameStats`. CPU time counts
preparation that ran after a commit, before the redraw, in the frame
that shows it (also when the drawable was not available). A report
covers one burst of frames: more than 250 ms of idle time (the gap
between frames less the host work in it: applying commits, preparing,
drawing) starts a new window, so sparse frames (a HUD painting a
report) never report on their own, while slow frames and slow commits
do.
`CRAIE_HEADLESS=1` runs the session with no window: the same `Ui`,
frame path, renderer (into an offscreen target), and statistics,
paced at 120 Hz by spinning, because a process with no visible window
gets coalesced timers on macOS (4 ms waits woke up to 30 ms late). It
is for measuring where no display is on. It sends the events a commit
raises at once, not with the next frame. Both loops own an image
decoder (`images.rs`): one worker thread that answers the core's
requests (`Ui::take_image_requests`: probe a header, decode a source
rect at a pixel size) with the `image` crate (PNG, JPEG, WebP, GIF's
first frame; EXIF orientation applied) and wakes the loop; results go
back through `Ui::image_result` after commits, and new requests go out
after each prepared frame. Probes run before decodes, a newer decode
of an image replaces its queued one, and queued work for images the
core dropped goes (`Ui::take_dropped_images`). Decoding is bounded:
over 64 megapixels fails at the header, and the decoder's buffers are
reserved against 512 MiB before it allocates (codecs may add about
one more image), and a WebP whose chunks claim more than the file
holds is not asked for its EXIF. A codec panic fails its image; a
worker that dies anyway is replaced. `craie_ui::platform` holds the contract: `WindowId`
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
- Images decode in the platform adapter with the `image` crate. The
  core receives prepared pixels and owns `ImageId`, dimensions, format,
  and residency. Built without a feature flag of our own (work item 8,
  2026-09-27): the `image` crate's format features (PNG, JPEG, WebP,
  GIF) are the switch, and every desktop host wants images. The core
  decides the decode size (the drawn size in device pixels, never
  upscaled), so a 4,000 px photo in a 40 pt box at 2x decodes to 80 px
  (25.6 KB of texture, not 48 MB).

## 16. Bridge and JS runtime

**Current.** As targeted. One process; React runs in a Node
`worker_thread` and submits CRW2 bytes through `NativeClient.submit`.
One threadsafe function delivers `ack | events` frames. JS recycles
ids at once and mirrors each slot's generation; events carry the
generation and JS drops stale ones; the ack resolves `flush()`.
Payload ops copy typed-array bytes once. Protocol version 10 (36-byte
event records and claims since 4; inherited color in drawings and
inputs since 5; the `switch`, `radio` and `radiogroup` roles and the
ROLE op's reported states since 6; press flags, pressable spans, and
`PRESS`/`ACTIVATE` since 7; focus traps, inert, auto-focus and the
`dialog` and `alertdialog` roles since 8; transform parts since 9;
focus groups and the `tab` and `tablist` roles since 10). The session hands JS its output in native
order: acks sit between event frames where they happened, so the ack
of a transaction never overtakes an event raised before it applied,
and the facade retires a claim set's old handlers on that ack.
Delivery is lossless where a promise or a user gesture waits: frames
carrying animation ends or claims never drop from the session's bounded queue
(the oldest droppable frame goes instead), and a frame the threadsafe
function's queue refuses waits in the session, in order, until the
next pump (JS calls `resume` after each frame it takes, which always
pumps: it waits for a pump that is storing a refused frame, then
retries it); a closed receiver closes the session.
`Layer` (work item 4) is a React portal into a layer container: a
full-window view at the end of the root level, opened when its first
child commits, owned by the enclosing `Layer`'s container. It closes
once it holds neither children nor an open layer it owns, then its
owner if that leaves it empty too. The app's root nodes are placed
before the first open layer.

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
harness and a workspace build as negative controls; E01 editable
cases (the owned editor against Parley's `PlainEditor` over scripted
steps: buffer, selection, preedit, and caret position equal after
every step; carets at graphemes where Parley splits them, asserted
exactly); Parley as a reference-only crate in every release graph; and
the E01, E10, and E14 benches (`examples/fontcost` splits a fresh
engine's font work). `prepare_frame` in
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
