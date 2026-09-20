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
React reconciler  (JS, not yet built)
       |  binary transactions, decoded off the UI thread
       v
host/             retained tree: 40-byte node headers in a Vec<NodeHeader>
layout/           Taffy queries Craie-owned storage (planned)
scene.rs          flat paint data: Vec<QuadInstance>, Vec<GlyphInstance>
text/             Parley layout -> Swash raster -> glyph cache -> atlas
gpu/              wgpu: 2 instanced pipelines, growable buffers, atlas arrays
platform/         winit window, Wait control flow, redraw on request
```

Data flows down. Nothing above `scene.rs` names wgpu; nothing above
`platform/` names winit.

## Retained host (`host/`)

Modeled on the packed host in `gpui-react` (commit `46cb47d`), which
measured ~0.7 ms to mount 5,000 nodes natively.

- `NodeHeader` is 40 bytes: five sibling/parent links, `child_count`, an
  `aux` row index into per-kind side tables, an interned `style` id, a
  `kind` index (top bit = hidden), a `generation` counter, and dirty
  flags. Run `cargo run --example sizes` to see the layout.
- Ids are assigned by the JS side and index the arena directly. Free
  slots carry `EMPTY_KIND`; there is no native free list because JS
  recycles ids after removal is acknowledged.
- `generation` bumps on reuse so a stale (id, generation) pair captured
  by an event listener can never reach the node that now occupies the
  slot.
- Children are a doubly-linked sibling list inside the header, so
  insertion and removal are O(1) with no per-node allocation.
- Sparse per-kind data (text content, scroll state, native components)
  lives in side tables addressed by `aux`, not in the header.
- `MutationBatch` is the unit of commit. It is a flat typed op list plus
  a string table, shaped so it can move to the packed binary wire without
  changing semantics.

## Text pipeline (`text/`)

Three specialists chained together, owned at the seams.

1. **Parley** lays out a `ParagraphSpec` (text + default styles + ranged
   spans) into a `Layout` that owns its shaped data and can be retained.
   Wrapping is `break_all_lines(max_width)`; on resize we relayout but do
   not re-rasterize.
2. **Swash** rasterizes only cache misses. Font identity is the font
   blob's `id()` plus face index; variation coordinates and synthetic
   styling (embolden, skew) are interned to compact slots.
3. **The glyph cache** keys on everything that changes the bitmap:
   interned font, interned coords/synthesis, glyph id, exact f32 size
   bits, and a quarter-pixel subpixel bucket (2 bits per axis).
   `GlyphKey` is 12 bytes.
4. **The atlas** is etagere-packed 2048x2048 pages, two sets: R8 alpha
   for masks, RGBA for color bitmap glyphs (emoji). Pages keep a CPU
   mirror plus a dirty rect; GPU upload writes only dirty regions. Glyphs
   get a 1 px gutter so linear filtering does not bleed.

`emit` walks a laid-out paragraph and appends `GlyphInstance` rows.
Re-emitting unchanged text does zero Swash work; reflow after resize
reuses every already-rasterized glyph.

## Paint (`scene.rs`)

The scene is three flat vectors, not a retained display list: quads,
glyphs, and a clear color. A `GlyphInstance` is 40 bytes of vertex-shader
input (position, size, uv rect, color, page, flags). Nothing per-node
becomes a GPU resource.

## GPU (`gpu/`)

- One device, one queue, two pipelines: instanced quads (triangle strip,
  `draw(0..4, instances)`) and instanced glyphs.
- Instance data lives in growable vertex buffers rewritten per frame;
  buffers only reallocate when capacity is exceeded.
- The atlas is mirrored on the GPU as two texture arrays
  (R8 + RGBA8, `D2Array`), grown in powers of two. Dirty CPU rects
  upload through `write_texture`; full-width rows skip repacking.
- All blending is premultiplied alpha; the glyph shader multiplies the
  sampled mask by the text color.

## Platform (`platform/`)

winit 0.31 is confined to `platform/winit.rs`. `Window` wraps
`Arc<dyn winit::window::Window>` and exposes only `size`,
`scale_factor`, `request_redraw`, and `surface_target` (which satisfies
wgpu's `WindowHandle` bound without naming winit elsewhere).

The event loop runs `ControlFlow::Wait`. Frames are produced only in
`RedrawRequested`, triggered by OS expose or an explicit request after
resize. A static app does no work per frame.

## Bridge direction (not yet built)

The gpui-react binary wire is the model: flat op stream with u8 tags,
positional schema-driven props with a presence mask, interned string
keys in a trailer, a serde `Deserializer` reading borrowed bytes, and
buffer pooling on the JS side. For Craie that maps to: React commits on a
worker thread encode mutations into a pooled buffer; a bounded queue
hands transactions to the UI thread; `EventLoopProxy::wake_up` pokes the
winit loop, which applies decoded ops to `Host` and requests one redraw.

## What is deliberately not here

No style engine beyond interned ids, no final component API, no
animation, no accessibility, no multi-window, no backend abstraction.
Taffy is a dependency but the layout module only defines the computed
rect so far. These come after the retained host is proven by the
5,000-row benchmark.
