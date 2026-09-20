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
       |  binary transactions over a localhost TCP socket
       v
bridge.rs         socket reader thread -> mpsc -> EventLoopProxy wake
wire.rs           flat op stream decode, applied to the host
ui.rs             facade: apply -> layout -> paint, one scene
host/             retained tree: 40-byte node headers in a Vec<NodeHeader>
layout/           Taffy low-level traits over host storage + text measure
scene.rs          flat paint data: Vec<QuadInstance>, Vec<GlyphInstance>
text/             Parley layout -> Swash raster -> glyph cache -> atlas
gpu/              wgpu: 2 instanced pipelines, growable buffers, atlas arrays
platform/         winit window, Wait control flow, redraw on request
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

`NodeHeader` is 40 bytes: five sibling/parent links, `child_count`, an
`aux` row index into per-kind side tables, an interned `style` id, a
`kind` index (top bit = hidden), a `generation` counter, and dirty
flags. Ids are JS-assigned and index the arena directly; free slots
carry `EMPTY_KIND`; `generation` bumps on reuse so stale references can
never reach the new occupant. Children are a doubly-linked sibling list
in the header — mutations allocate nothing.

Sparse per-kind data lives in side tables addressed by `aux`: `TextRow`
(text, font size, color) for `TEXT`, `ViewRow` (background color) for
`VIEW`.

## Layout (`layout/`)

Taffy 0.14 is used through its low-level traits (`TraversePartialTree`,
`LayoutPartialTree`, `CacheTree`, `RoundTree`,
`LayoutFlexboxContainer`) — it reads children and interned styles
straight out of the host and writes results into `Layouts`, a parallel
per-node store (cache, unrounded layout, final rect + content-box
offset). There is no second authoritative tree.

Dirty propagation is explicit: a node's Taffy cache key covers only its
own inputs, so a mutation clears the cache on the node and every
ancestor (`invalidate`). `clear_all_caches` covers style redefinition
and resizes.

TEXT leaves are measured through Parley inside the leaf measure
callback; the resulting `Layout<Color>` is retained per node
(`MeasuredText`) and re-emitted at paint without reshaping unless the
text or its wrap width changed.

## Paint (`ui.rs`, `scene.rs`)

`Ui::render` computes layout for each root, then does a pre-order DFS
over sibling links accumulating parent content-box origins — no per-node
allocation. Views with a non-transparent background emit one
`QuadInstance`; text nodes emit `GlyphInstance` rows from their retained
Parley layout. The scene is three flat vectors rebuilt in place; the GPU
upload of each buffer is tracked separately.

## Text pipeline (`text/`)

Parley lays out a `ParagraphSpec` into a `Layout` that owns its shaped
data. Swash rasterizes only cache misses. The glyph cache keys on
interned font identity, interned coords/synthesis, glyph id, exact f32
size bits, and quarter-pixel subpixel buckets. The atlas is
etagere-packed 2048x2048 pages, R8 + RGBA, with CPU mirrors and dirty
rects; GPU upload writes only dirty regions.

## GPU (`gpu/`)

One device, two instanced pipelines, growable vertex buffers, atlas
texture arrays. Two draw calls per frame regardless of node count.

## Platform (`platform/`) and bridge (`bridge.rs`)

winit 0.31 is confined to `platform/winit.rs`. The event loop runs
`ControlFlow::Wait`; frames are produced only in `RedrawRequested`.
`Wake` wraps `EventLoopProxy::wake_up` (payload-less in 0.31 — apps pair
it with their own channel, delivered to `App::woke`).

`bridge::listen` binds a localhost TCP socket; a reader thread per
connection frames `u32 len | txn` payloads into an mpsc channel and
wakes the loop. The main thread drains the inbox in `woke`, applies
transactions, and repaints once. The socket thread never touches host
state. TCP stands in for the napi shared buffer — same properties that
matter (opaque bytes, wake-based delivery, no JS on the main thread),
zero toolchain.

## JS side (`packages/bridge`)

- `wire.ts`: byte-exact encoder mirror (writer, string interning,
  positional style encoding, style dedup).
- `host.ts`: JS-owned dense ids with a free list, prop diffing to
  minimal ops, `queueMicrotask` seal per commit.
- `index.ts`: react-reconciler 0.33 mutation-mode config; `<View>` and
  `<Text>` components; `shouldSetTextContent` absorbs string children
  into the text prop.
- `client.ts`: `u32 len | payload` TCP transport.

## What is deliberately not here

No events/hit-testing, no scrolling, no text input, no animation, no
accessibility, no multi-window, no `<Image>`. Per-subtree paint damage
and layout damage regions are not implemented — `paint` re-emits the
whole scene and the root flex pass re-runs on any change (see
EXPERIMENTS for what that costs at 5,000 rows).
