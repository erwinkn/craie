# Craie architecture

Craie is a prototype for a React-driven native desktop renderer. React is
authoritative for composition; native code retains only the state it needs
to produce pixels. The bet: owning the retained representation and the
renderer directly is a better fit for a React host than GPUI's retained
entity model, where the bridge had to translate React mutations into
GPUI's world.

The rule throughout: own exactly the state Craie needs, use specialized
libraries for the hard algorithms (Parley for text layout, Swash for
rasterization, etagere for atlas packing, Taffy for flex), and keep the
hot representation compact enough that its cost is obvious.

## Layers

```
React reconciler  (packages/bridge — JS, react-reconciler mutation mode)
       |  binary transactions submitted in-process (worker thread)
       v
crates/node       N-API: NativeClient.submit -> Session queue -> wake
bridge.rs         bounded commit queue + ack queue (Session)
wire.rs           flat op stream decode, applied to the host
ui.rs             facade: apply -> layout -> paint, one scene
host/             retained tree: 20-byte node headers in a Vec<NodeHeader>
layout/           Taffy low-level traits over host storage + text measure
scene.rs          one ordered Vec<Instance> — quads and glyphs unified
text/             Parley layout -> Swash raster -> glyph cache -> atlas
gpu/              wgpu: 1 instanced pipeline, growable buffer, atlas arrays
platform/         winit window, Wait control flow, redraw on request
app.rs            the shared host app: session -> ui -> surface -> present
```

Data flows down. Nothing above `scene.rs` names wgpu; nothing above
`platform/` names winit.

## The wire (`wire.rs`, `packages/bridge/src/wire.ts`)

Modeled on gpui-react commit `46cb47d`. One React commit is one
transaction:

```
magic "CRW1" u32 | version u16 | flags u16 | seq u64 | string_count u32
strings: count x (u32 len + utf8)      -- referenced by index in ops
ops:     flat u8-tagged records to EOF
```

Ops: `create`, `set_text`, `text_props`, `set_style`, `place`, `detach`,
`remove`, `hidden`, `view_paint`, and `style` (a style *definition*:
u64 presence mask + positional fields — display, position, flex-*, the
alignment keywords, gap, size/min/max, padding/margin/border/inset
rects, flex basis/grow/shrink, aspect ratio, overflow).

Decoding borrows strings from the buffer and produces typed ops with no
intermediate value objects. On the JS side, style objects are interned
by canonical JSON key so identical styles share one wire id and one
native `taffy::Style` record. The format and a JS fixture are checked by
`crates/craie/tests/wire_fixture.rs`.

## Retained host (`host/`)

`NodeHeader` is 20 bytes: a `parent` link, an `aux` row index into
per-kind side tables, a wire `style` id, a `kind` index (top bit =
hidden), a `generation` counter, and dirty flags. Ids are JS-assigned
and index the arena directly; free slots carry `EMPTY_KIND`;
`generation` bumps on reuse so stale references can never reach the new
occupant. Children live in a `Vec<NodeId>` side table parallel to the
arena (plus a `roots` list) — indexed access for Taffy, no sibling
links, empty vecs allocate nothing.

Dirty tracking is a queue: a LAYOUT transition pushes the node onto
`layout_dirty` once, and the layout pass drains it and walks ancestors —
no arena-wide flag scans.

Sparse per-kind data lives in side tables addressed by `aux`: `TextRow`
(text, font size, color) for `TEXT`, `ViewRow` (background color) for
`VIEW`.

## Layout (`layout/`)

Taffy 0.14 is used through its low-level traits (`TraversePartialTree`,
`LayoutPartialTree`, `CacheTree`, `RoundTree`,
`LayoutFlexboxContainer`) — it reads children and wire styles straight
out of the host and writes results into `Layouts`, a parallel per-node
store (cache, unrounded layout, final rect + content-box offset). There
is no second authoritative tree.

All of this is in **logical units**: styles, wrap widths, rects. The
display scale enters only at emit, where glyph origins and raster sizes
convert to physical pixels, so a monitor-scale change never reshapes.

Dirty propagation is explicit: a node's Taffy cache key covers only its
own inputs, so a mutation clears the cache on the node and every
ancestor (`invalidate`, driven by the host's dirty queue).
`clear_all_caches` covers style redefinition and resizes.

TEXT leaves are measured through Parley inside the leaf measure
callback; the resulting `Layout<Color>` is retained per node
(`MeasuredText`) and re-emitted at paint without reshaping unless the
text or its wrap width changed.

## Paint (`ui.rs`, `scene.rs`)

`Ui::render` computes layout for each root, then does a pre-order DFS
accumulating parent content-box origins (logical units). Nodes whose
border box misses the viewport emit nothing; children are still
visited. The scene is ONE `Vec<Instance>` in document order — painter
order is vector order — uploaded to a single growable buffer and drawn
in a single draw call.

Each text node keeps an `EmittedText` batch: physical-pixel instances
keyed on scale, origin, and color. An unchanged repaint replays the
batch with zero shaping, rasterization, or glyph-cache lookups; a
color-only change rewrites instance colors in place.

## Text pipeline (`text/`)

Parley lays out a `ParagraphSpec` into a `Layout` that owns its shaped
data. Swash rasterizes only cache misses. The glyph cache keys on
interned font identity, interned coords/synthesis, glyph id, exact f32
size bits, and quarter-pixel subpixel buckets. The atlas is
etagere-packed 2048x2048 pages, R8 + RGBA, with CPU mirrors and dirty
rects; GPU upload writes only dirty regions.

## GPU (`gpu/`)

One device, one instanced pipeline, one growable vertex buffer, atlas
texture arrays. One draw call per frame regardless of node count —
`Instance` unifies rects (`FLAG_SOLID`, no fetch) and glyphs (atlas
sample), so blending order follows instance order.

## Platform (`platform/`), bridge (`bridge.rs`), and node (`crates/node`)

winit 0.31 is confined to `platform/winit.rs`. The event loop runs
`ControlFlow::Wait`; frames are produced only in `RedrawRequested`.
`Wake` wraps `EventLoopProxy::wake_up` (payload-less in 0.31 — apps pair
it with their own channel, delivered to `App::woke`).

The runtime is one process: the main thread owns the winit event loop
(via `app.rs`'s `HostApp`), and React runs in a `worker_thread`.
`NativeClient.submit(bytes)` copies the encoded transaction into the
`Session`'s bounded commit queue and wakes the loop; `woke` drains,
applies each transaction atomically, acks its seq, and repaints once if
needed. Acks return to the worker through a blocking `receive` N-API
task over the session's condvar. No sockets, no shared memory — one
copied commit per React transaction.

Acks close the loop for id ownership: JS holds removed ids until the
transaction that removed them is acknowledged, then recycles them.
Reuse can never race native apply, and `Root.flush()` resolves only
after the frame is confirmed.

## JS side (`packages/bridge`)

- `wire.ts`: byte-exact encoder mirror (writer, string interning,
  positional style encoding, style dedup).
- `host.ts`: JS-owned dense ids with a free list gated on native acks,
  prop diffing to minimal ops, `queueMicrotask` seal per commit.
- `index.ts`: react-reconciler 0.33 mutation-mode config; `<View>` and
  `<Text>` components; `shouldSetTextContent` absorbs string children
  into the text prop.
- `native.ts`: `NativeTransport` over `NativeClient` (submit + ack
  pump), `runApp` (main thread: host + worker + event loop) and
  `attachApp` (worker side).

## What is deliberately not here

No events/hit-testing, no scrolling, no text input, no animation, no
accessibility, no multi-window, no `<Image>`. Per-subtree paint damage
and layout damage regions are not implemented — `paint` re-walks the
tree each repaint (emitting only viewport-visible instances) and the
root flex pass re-runs on any change (see EXPERIMENTS for what that
costs at 5,000 rows).
