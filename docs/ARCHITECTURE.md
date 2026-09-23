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
craie-platform-winit  ----->  craie-ui  ----->  craie-text
      |                           |                 |
      +-------------------->  craie-scene  <--------+
                                  |
                              craie-render (wgpu)
                                  |
                              craie-core
```

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

Current state: everything sits in one crate, `crates/craie`, with
`crates/node` for N-API. The file map is:

```
bridge.rs     bounded commit queue + ack/event outbox (Session)
wire.rs       CRW1 decode, validate, apply
ui.rs         facade: apply -> layout -> paint; dispatch, focus, scroll
host/         20-byte NodeHeader arena, per-kind side tables
layout/       Taffy low-level traits over host storage
scene.rs      one ordered Vec<Instance>, rects and glyphs unified
text/         Parley layout -> Swash raster -> glyph cache -> atlas
input.rs      Parley PlainEditor per INPUT node
events.rs     normalized events + outbound frame encoding
custom.rs     registered painters for CUSTOM nodes
a11y.rs       AccessKit projection, full republish
gpu/          wgpu: one instanced pipeline, one growable buffer
platform/     winit 0.31-beta window, Wait control flow
app.rs        session -> ui -> surface -> present
```

---

## 1. Workspace and crates

**Current.** One library crate plus the N-API crate. No feature flags.
No WASM target. `ui.rs` is 2,100 lines and owns apply, layout, paint,
dispatch, focus, and scroll.

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

## 2. Wire and mutation executor

**Current.** CRW1: magic, version, flags, seq, string table, then flat
u8-tagged ops. Ops: create, set_text, text_props, set_style, place,
detach, remove, hidden, paint, props, input_props, custom, label,
command, style definition (u64 presence mask + positional fields). A
transaction is validated fully before any op applies. Styles are
interned by canonical JSON key in a JS map that lives forever, and
native stores one `taffy::Style` per wire style id. A `hidden` op and
`display: none` both hide a node.

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

## 3. Host tree and storage

**Current.** `NodeHeader` is 20 bytes: parent, aux row, style id, kind
with a hidden bit, generation, flags. Ids are JS-assigned arena
indices. Children live in a `Vec<NodeId>` side table parallel to the
arena. Sparse per-kind rows: `TextRow` (text, font size, color),
`ViewRow` (fill, radius, border). Truly sparse state (listeners,
focusability, scroll offsets, labels) lives in id-keyed maps on `Ui`.
Dirty tracking is a queue; a flag on the header dedupes pushes.

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

## 4. Layout

**Current.** Taffy 0.14 through its low-level traits over host storage.
Results land in `Layouts`, a per-node store (cache, unrounded layout,
final rect, content-box offset). Everything is in logical units. A
mutation clears the Taffy cache on the node and its ancestors. Text
leaves are measured through Parley in the leaf callback. Cold layout of
10k nodes costs 217 to 567 ms; warm unchanged costs 0.06 ms; one
streaming append costs 0.65 ms.

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
`View` defaults to `display: flex`, column direction, stretch, as in
React Native. `block` and `grid` are explicit values.

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

**Current.** Parley lays out a `ParagraphSpec` into a retained
`Layout`; Swash rasterizes cache misses; the glyph cache keys on
interned font id, interned coords/synthesis, glyph id, exact size bits,
and quarter-pixel subpixel buckets. Each text node keeps a
`MeasuredText` (shaped layout) and an `EmittedText` batch of
physical-pixel instances keyed on scale, origin, and color. Retained
Parley layouts are the memory floor: about 27 MiB for 5k rows. INPUT
nodes hold a Parley `PlainEditor` each. CJK line breaking degrades
because ICU4X segmentation data is not bundled.

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

## 6. Fonts

**Current.** Parley's fontique handles discovery and fallback. Font
identity is `FontData.data.id()` plus face index, interned to a u16 in
the glyph cache.

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
content extent. No virtualization: layout visits every node.

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

## 8. Scene

**Current.** One `Vec<Instance>` in document order, rebuilt by a
pre-order walk each repaint. `Instance` is 80 bytes and unifies rect
fills, borders, corner radii, glyph quads, and a per-instance clip
rect. Coordinates are physical pixels with absolute origins. One draw
call. The warm walk costs 0.03 ms for 10k nodes; 1,101 instances reach
the GPU after viewport culling.

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

**Current.** etagere-packed 2048x2048 pages, R8 and RGBA, CPU mirrors
with dirty rects, dirty-region uploads. A page cap plus an LRU over
glyph entries. Allocation failure evicts least-recently-used
allocations and marks cache entries absent.

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

## 11. GPU renderer

**Current.** wgpu 30. One device, one instanced pipeline, one growable
vertex buffer, atlas texture arrays. One draw call per frame. Colors
decode from sRGB to linear at emit; blending happens in linear.

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
Roles are inferred from listeners and overflow. The whole tree
republishes on any a11y-observable change. Actions queue back onto the
UI thread. The adapter is a vendored fork patched for winit 0.31 beta.

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

**Current.** winit 0.31.0-beta.3 confined to `platform/winit.rs`,
`ControlFlow::Wait`, frames only on `RedrawRequested`. Wake wraps
`EventLoopProxy::wake_up`. `martensite-accesskit-winit` 0.18 (fork).
One window.

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

**Current.** One process. The main thread owns the winit loop and the
retained `Ui`. React runs in a Node `worker_thread` and calls
`NativeClient.submit(bytes)`, which copies the transaction into a
bounded queue and wakes the loop. One threadsafe function delivers
`ack | events` frames. JS holds removed ids until ack. Apps bundle with
esbuild and run under Node because Bun cannot require N-API addons in
workers. Decode plus apply is 0.5 ms for 10k nodes; the session copy is
0.01 ms for 683 KB.

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
frame: apply, layout, shaping, raster, paint, upload, submit.

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

**Current.** `cargo test` covers host, wire validation, layout,
dispatch, input editing, a11y projection, atlas eviction, and one
cross-language wire fixture. `bun test` covers the encoder and the
reconciler. Benchmarks live in `examples/bench` and
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

**Current.** No WASM build.

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
