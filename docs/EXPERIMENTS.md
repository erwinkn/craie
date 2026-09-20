# Experiments

Milestone-0 findings. Each entry is a decision or measurement with the
reason it landed.

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

First `sync_atlas` uploads whole pages: 1 alpha page (4 MiB) + 1 color
page (16 MiB) = 20 MiB in the demo's first frame. Subsequent updates
upload only etagere dirty rects. The initial whole-page upload is
wasteful (the pages are mostly empty) and could be restricted to the
union of allocated rects. Noted, not fixed; it costs one large copy at
startup.

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

The 5,000-row benchmark is still TODO. The structural numbers are in
place to compare against it.

## Verification so far

- `cargo test`: 8 host tests (ordering, splicing, recycling, generation,
  hidden bit, header size).
- `cargo run --example text -- --screenshot out.png`: renders correctly,
  including shaped RTL Arabic, CJK, color emoji, and a styled-run
  paragraph. Verified visually.
- Idle behavior is by construction (`ControlFlow::Wait` + redraw only on
  request) but has not been measured with a live window.
