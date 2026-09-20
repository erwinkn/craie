# Experiments

Findings per milestone. Each entry is a decision or measurement with the
reason it landed.

## 5,000-row benchmark (`examples/bench`, release build)

A root column with 5,000 rows, each a styled view containing one text
node (10,001 nodes total). Numbers on this machine (M-series, 1600x1000
@2x):

| phase                        | time      | allocs  | notes |
|------------------------------|-----------|---------|-------|
| encode txn (Rust side)       | 10.5 ms   | 10,031  | JS-side analogue |
| wire bytes                   | 683 KB    |         | ~68 B/node incl. strings |
| decode + apply               | 0.47 ms   | 5,057   | ~47 ns/node — strings + rows |
| layout + paint, cold         | 456 ms    | 516k    | Parley shapes 5,000 paragraphs |
| layout + paint, warm/clean   | 205 ms    | 5,004   | re-runs root flex + re-emits 222k glyphs |
| apply 500 `set_text`         | 0.02 ms   | 9       | |
| layout + paint, 500 updates  | 261 ms    | 52k     | 500 re-measures + root relayout |

Retained state: 10,001 nodes x 40 B = 390 KiB of headers; live heap ~70
MB, dominated by 5,000 retained Parley `Layout`s (shaping output is the
heavy retained state, not our rows) and the 222k-row glyph instance
vector.

What this says:

- **The wire path is free.** Decode+apply is 0.5 ms for 10k nodes and
  0.02 ms for 500 updates. The retained host is not the bottleneck.
- **Layout is incremental but paint is not.** A 500-row update re-runs
  the root flex pass (Taffy caches the leaves) and the scene rebuild
  re-emits every glyph instance. At list scale, the ~200 ms warm repaint
  is the dominant cost and it is *O(visible + invisible nodes)* — a
  real app needs a viewport/virtualized list or subtree damage tracking
  before this matters, which is exactly what a Marbre-like UI would need.
- **Retained Parley layouts are the memory floor.** ~70 MB live for 5k
  short rows. If text memory matters, keep shaped layouts only for the
  visible window.
- **encode 10 ms for 10k nodes in Rust**; the TS encoder will differ but
  the op count is the same.

## Text stack: Parley + Swash + etagere

Chosen over GPUI's cosmic-text-style approach because the split matches
what Craie owns: Parley does shaping, bidi, and line breaking; Swash does
rasterization only; Craie owns the cache key and the atlas. The seams are
clean. Parley's `GlyphRun` exposes everything the cache key needs
(`run.font()`, `font_size()`, `normalized_coords()`, `synthesis()`), and
font identity is `FontData.data.id()` + face index, so interning is cheap.

Measured on the `text` example (1800x1200 @2x, multilingual + emoji +
styled runs): 41 glyph runs, 443 glyph instances, 300 rasterizations on
first emit. The cache absorbs the rest. Re-emit after reflow rasterizes
nothing new.

### ICU4X segmentation

Parley warns `No segmentation model for complex script: Chinese/Japanese`
because the ICU4X line-segmentation data is not bundled. CJK glyphs still
shape and render; what degrades is line breaking inside CJK text (breaks
fall back to space-separator rules). Options: enable the `icu` feature on
Parley (pulls compiled segmentation data), or accept degraded CJK
wrapping until the demo needs it. Deferred.

### Subpixel positioning

The cache quantizes glyph offsets to quarter pixels (2 bits per axis,
baked into `GlyphKey`). Whole-pixel positions share a bitmap; fractional
positions rasterize at the fractional offset. Rounding is `round()` not
`trunc()`, so the bucket 4 case folds into the next integer.

### Atlas upload cost

Each page tracks a `used` union of everything ever blitted. Fresh
texture arrays (first sync, page-array growth) upload `used` per page —
60 KiB on the wire-demo's first frame, down from 20 MiB of whole pages.
Steady-state updates upload only etagere dirty rects.

## Renderer

Two instanced draw calls per frame regardless of node count: quads
(`TriangleStrip`, `draw(0..4, n)`) and glyphs. Instance rows are
`Pod` structs (`QuadInstance` 20 B, `GlyphInstance` 40 B) uploaded into
growable vertex buffers. No per-node GPU objects, no draw call per node.

### wgpu 30 API changes that bit

- `Surface::get_current_texture` returns a `CurrentSurfaceTexture` enum
  (Success / Suboptimal / Timeout / Occluded / Outdated / Lost /
  Validation), not `Result`. Present happens on `Drop`, not
  `present()`.
- Pipeline layouts take `Option<&BindGroupLayout>` entries and vertex
  buffers take `Option<VertexBufferLayout>`.
- `RenderPassDescriptor` has a `multiview_mask` field.
- `SurfaceConfiguration` has a `color_space` field
  (`SurfaceColorSpace::Auto` keeps the old behavior).
- `InstanceDescriptor` has no `Default`; use
  `new_without_display_handle_from_env()` as the base.
- `BufferSlice::get_mapped_range` returns `Result`.

### winit 0.31 API changes that bit

- `Window` and `ActiveEventLoop` are traits;
  `create_window` returns `Box<dyn Window>`.
- Surface creation moves to `ApplicationHandler::can_create_surfaces`
  (the `resumed` split is gone).
- `WindowEvent::Resized` is `SurfaceResized`.
- Frames come from `RedrawRequested` only; with `ControlFlow::Wait` a
  static app sleeps.

## Retained host

Copied from gpui-react commit `46cb47d` ("schema-driven binary wire and
pack native rows"), which measured ~0.7 ms native mount of 5,000 nodes
and cut the wire from ~825 KB to ~435 KB vs JSON.

`NodeHeader` is 40 bytes (see `cargo run --example sizes`). The main
differences from a naive `Rc`-tree: JS-assigned dense ids index the arena
directly, `EMPTY_KIND` marks free slots instead of a native free list,
`generation` makes stale references safe, and children are sibling links
in the header so mutations allocate nothing.

Measured: decode+apply of the 10k-node mount txn takes 0.47 ms — the
packed host is not where frame time goes.

## Taffy integration

Taffy runs over the host through its low-level traits; it never owns a
node. `Layouts` holds interned `taffy::Style`s plus three parallel
per-node stores (`Cache`, unrounded `Layout`, final `LayoutData`).

- The measure callback receives content-box `AvailableSpace` — wrap
  width is `available.width` when definite, no padding subtraction
  needed.
- A leaf whose cache key misses gets re-measured; the retained Parley
  layout is keyed on (text dirty | wrap width bits), so unchanged text
  in a relayout emits without reshaping.
- Dirty = flag on the node + walk ancestors clearing their Taffy caches
  (a node's cache key doesn't include children).
- `layout.location` is relative to the parent's *content* box; paint
  accumulates `parent.content_origin` (border + padding) while walking.

## Wire + JS bridge

- The wire is self-describing via op tags and the style presence mask;
  there is no schema negotiation. The JS encoder and Rust decoder are
  pinned to the same constants by `tests/wire_fixture.rs` (fixture
  generated by `packages/bridge/scripts/gen-fixture.ts`).
- `queueMicrotask` seals one transaction per commit — React batches
  inside a commit already, so a commit maps to exactly one socket frame.
- E2E verified: `cargo run --example app` + `bun examples/js/demo.tsx`
  renders a Marbre-style chat layout (sidebar, message list, composer)
  ticking once a second; each tick is one ~200-byte transaction applied
  and painted once.

## Verification so far

- `cargo test`: 14 lib tests (host, wire, ui: resize reflow +
  hidden-sibling paint) + 1 cross-language fixture test.
- `bun test` in `packages/bridge`: 6 tests (golden wire bytes, style
  mask round-trip, reconciler mount/update ops, ack-gated id recycling).
- `cargo run --example text -- --screenshot`: multilingual render incl.
  RTL Arabic, CJK, color emoji — verified visually.
- `cargo run --example app -- --screenshot`: wire-built UI rendered
  through the full apply -> Taffy -> Parley -> GPU path.
- Ack round-trip verified over the live socket: `Root.flush()` resolves
  only after the app applies the transaction.
- Idle is by construction (`Wait` + redraw only on request). The bridge
  path wakes the loop only when a frame arrives; measured indirectly —
  between ticks the app logs nothing.
