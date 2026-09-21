# Experiments

Findings per milestone. Each entry is a decision or measurement with the
reason it landed.

## 5,000-row benchmark (`examples/bench`, release build)

A root column with 5,000 rows, each a styled view containing one text
node (10,001 nodes total). Numbers on this machine (Apple M5 Max,
1600x1000 @2x), phases measured independently:

| phase                        | time      | allocs  | notes |
|------------------------------|-----------|---------|-------|
| encode txn (Rust side)       | 11.3 ms   | 10,031  | JS-side analogue |
| wire bytes                   | 683 KB    |         | ~68 B/node incl. strings |
| decode + apply               | 0.54 ms   | 10,095  | ~54 ns/node — strings + rows |
| layout, cold                 | 216.6 ms  | 476k    | Taffy flex + Parley shapes 5,000 leaves |
| — compute vs rounding        | 215.8 / 0.07 ms |  | rounding is free |
| paint + emit, cold           | 1.81 ms   | 263     | 1,101 instances after viewport culling |
| paint, warm unchanged        | 0.04 ms   | 12      | replays retained EmittedText batches |
| layout, warm unchanged       | 0.06 ms   | 0       | Taffy cache absorbs the whole tree |
| Taffy cache                  | 10,001 hits / 75,001 misses | | cold-run counters |
| apply 500 `set_text`         | 0.02 ms   | 17      | |
| layout, 500 dirty            | 10.7 ms   | 44.5k   | 500 re-measures + root relayout |
| paint, 500 dirty             | 0.19 ms   | 33      | only dirty text re-emits |
| commit copy + drain          | 0.01 ms   | 3       | 683 KB txn through Session |
| atlas upload                 | 0.14 ms   | 47      | 105,728 bytes |
| draw submit, first           | 0.66 ms   | 85      | buffer upload + 1 draw call |
| GPU completion, first        | 21.8 ms   | 3       | first-submit warmup |
| draw submit, warm            | 0.18 ms   | 51      | |
| GPU completion, warm         | 0.70 ms   | 0       | |

Retained state: 10,001 nodes x 20 B = 195 KiB of headers; live heap ~35
MiB, dominated by 5,000 retained Parley `Layout`s (shaping output is the
heavy retained state, not our rows).

## Marbre-like transcript (2,000 messages + 300 stream ticks)

A chat-shaped tree — sidebar, 2,000 message rows with mixed text, a
composer — followed by 300 single-message-append transactions:

| phase                        | time      | allocs  |
|------------------------------|-----------|---------|
| mount txn                    | 372 KB    |         |
| decode + apply               | 0.54 ms   | 8,595   |
| layout, cold                 | 125.5 ms  | 313k    |
| paint + emit, cold           | 1.32 ms   | 306     |
| paint, warm unchanged        | 0.03 ms   | 10      |
| 300 stream txns total        | 203 ms    | 74k     |
| — per-txn avg: apply         | 0.000 ms  |         |
| — per-txn avg: layout        | 0.645 ms  |         |
| — per-txn avg: paint         | 0.031 ms  |         |

What this says:

- **The wire path is free.** Decode+apply is 0.5 ms for 10k nodes; the
  in-process session copy is 0.01 ms for a 683 KB commit. A bounded
  queue beats a shared-memory ring at these sizes — no further transport
  work is justified.
- **Taffy layout is the whole story.** Cold layout is ~120–220 ms for
  10k nodes; incremental layout is ~0.65 ms per streaming append. Paint,
  emit, upload, and GPU completion are all noise next to it. Any future
  optimization effort goes to layout (or to not laying out — see below).
- **Warm frames cost nothing.** An unchanged repaint replays retained
  `EmittedText` batches: 0.03–0.04 ms, ~10 allocs, zero shaping,
  rasterization, or glyph-cache lookups. Taffy's cache makes warm
  layout 0.06 ms. Keeping the cache is justified; replacing it buys
  nothing.
- **Retained Parley layouts are the memory floor.** ~35 MiB live for
  5k–10k short rows. The 20 B header is 0.5% of that. If text memory
  matters, the fix is windowing shaped layouts to the visible range —
  not shrinking our own storage.
- **Viewport culling already does the heavy lifting.** 1,101 instances
  reach the GPU from a 10k-node tree. The remaining cost is that
  *layout still visits every node*; a virtualized list or subtree
  damage tracking is the next lever when transcript-scale UIs land.
- **encode 11 ms for 10k nodes in Rust**; the TS encoder differs but
  the op count is the same.

## Frame-cost comparison (`examples/framebench`)

The craie counterpart of gpui-react's `fixtures/performance`: same
scene (800x600, 32-px status line, N identical 20-px text rows, `flow`
retained shape), same protocol (mount, first draw, 10 warmup + 100
measured iterations of a one-op status update + draw, then scroll
steps, then removal + empty draw, live bytes per phase). Transactions
are real React commits — `examples/js/dump-framebench.tsx` renders the
scene through `createRoot`/`renderSync` and captures the sealed frames;
`framebench` replays them. `scripts/measure-framebench.sh` sweeps
100/1k/5k rows x 3 reps.

Two substitutions, matching what Craie can do today:

- **No `list` scene.** List virtualization is planned work; only the
  retained-everything `flow` comparison exists.
- **`scrollAndDraw` replaces `wheelAndDraw`.** Craie has no input
  events or native scroller yet (both planned), so a scroll step
  applies a React-driven `marginTop` change on the content view — the
  mechanism a craie app ships today. After the first step defines the
  style, each scroll txn is a single `set_style` op, exactly what the
  reconciler emits.

Medians on this machine (GPU submit included in `draw`, completion
excluded — same boundary as the fixture):

| phase        | 100 rows | 1,000 rows | 5,000 rows |
|--------------|----------|------------|------------|
| mount        | 0.02 ms  | 0.05 ms    | 0.23 ms    |
| firstDraw    | 8.2 ms   | 8.4 ms     | 37 ms      |
| update.apply | ~0 ms    | ~0 ms      | ~0 ms      |
| update.draw  | 0.16 ms  | 0.15 ms    | 0.13 ms    |
| scroll.apply | ~0 ms    | ~0 ms      | ~0 ms      |
| scroll.draw  | 0.12 ms  | 0.30 ms    | 1.15 ms    |
| remove       | 0.03 ms  | 0.18 ms    | 1.3 ms     |

Live bytes (5k rows): +962 KiB at mount, +26.6 MiB at first draw
(retained Parley layouts), ~+350 KiB over the update phase, −16 MiB
freed at removal.

Note the row shape differs from the synthetic bench above: rows are
bare fixed-height texts (5004 nodes), so Taffy skips the measure
callback and Parley shaping lands entirely in `firstDraw`.

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

Shaped Parley layouts are retained per text node (`MeasuredText`, 328 B
slot + heap). A dirty text re-measures inside the leaf callback; a clean
text in a relayout re-emits without reshaping. On top of that, each node
keeps an `EmittedText` batch — physical-pixel instances keyed on
(scale, origin, color) — so an unchanged repaint does zero text work,
and a color-only change rewrites instance colors in place. Color is not
part of the layout key; `set_color` marks TEXT-free paint dirty only.

Swash scalers are created lazily — a cache hit never constructs one.

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
`trunc()`, so the bucket 4 case folds into the next integer. Coord
interning is a small linear-scan table — no per-run `Box` allocations.

### Atlas upload cost

Each page tracks a `used` union of everything ever blitted. Fresh
texture arrays (first sync, page-array growth) upload `used` per page —
60 KiB on the wire-demo's first frame, down from 20 MiB of whole pages.
Steady-state updates upload only etagere dirty rects. Measured at bench
scale: 0.14 ms and 105 KB for the whole first frame.

## Renderer

One instanced draw call per frame regardless of node count. Quads and
glyphs share a 40-byte `Instance` row in a single document-ordered
`Vec` — `FLAG_SOLID` marks a rect (no atlas fetch), `FLAG_COLOR` a color
bitmap glyph — uploaded to one growable vertex buffer. Paint order is
vector order; the old quad-then-glyph split (glyphs could never paint
under a later quad) is gone. Nodes outside the viewport emit nothing.

Measured: 0.18 ms warm submit + 0.70 ms warm completion. The 21.8 ms
first-frame GPU completion is Metal warmup, not steady state. wgpu adds
no measurable overhead at this scene size.

### wgpu 30 API changes that bit

- `Surface::get_current_texture` returns a `CurrentSurfaceTexture` enum
  (Success / Suboptimal / Timeout / Occluded / Outdated / Lost /
  Validation), not `Result`. Present is explicit —
  `queue.present(frame)` after `window.pre_present_notify()`; dropping
  the frame discards it.
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

`NodeHeader` is 20 bytes: `parent` link, `aux` side-table row, wire
`style` id, `kind` index (+hidden bit), `generation`, dirty flags. Ids
are JS-assigned and index the arena directly; `EMPTY_KIND` marks free
slots; `generation` bumps on reuse so stale references can never reach
the new occupant. Children live in a `Vec<NodeId>` side table (plus a
`roots` list): indexed O(1) access for Taffy, no sibling links, empty
vecs allocate nothing.

Dirty tracking is a queue, not a scan: a LAYOUT transition pushes the
node onto `layout_dirty` once; the layout pass drains it and walks
ancestors. Local updates never touch the other 9,999 nodes.

Measured: decode+apply of the 10k-node mount txn takes 0.54 ms; 500
text updates apply in 0.02 ms.

## Taffy integration

Taffy runs over the host through its low-level traits; it never owns a
node. `Layouts` holds the decoded `taffy::Style`s indexed by wire id —
JS already dedupes styles, so there is no native interning pass — plus
three parallel per-node stores (`Cache`, unrounded `Layout`, final
`LayoutData`).

- The measure callback receives content-box `AvailableSpace` — wrap
  width is `available.width` when definite, no padding subtraction
  needed.
- A leaf whose cache key misses gets re-measured; the retained Parley
  layout is keyed on (text dirty | wrap width bits), so unchanged text
  in a relayout emits without reshaping.
- Dirty = bit on the node + queue push + walk ancestors clearing their
  Taffy caches (a node's cache key doesn't include children). Text
  color does NOT set layout-dirty; text content and font do.
- `layout.location` is relative to the parent's *content* box; paint
  accumulates `parent.content_origin` (border + padding) while walking.
- Cache behavior measured: 10,001 hits / 75,001 misses on the cold run;
  warm unchanged layout is 0.06 ms. Rounding is 0.07 ms of a 216 ms
  pass — `RoundTree` is free. The cache stays.
- All coordinates are logical points; the display scale enters only at
  emit, where glyph origins and raster sizes go physical. A scale change
  invalidates emitted batches, not shaped layouts.

## Wire + JS bridge

- The wire is self-describing via op tags and the style presence mask;
  there is no schema negotiation. The JS encoder and Rust decoder are
  pinned to the same constants by `tests/wire_fixture.rs` (fixture
  generated by `packages/bridge/scripts/gen-fixture.ts`).
- `queueMicrotask` seals one transaction per commit, and a commit maps
  to exactly one `Session::submit` — one copied `Vec<u8>` into a bounded
  queue (256 txns / 4 MiB), one `Wake` to the event loop.
- Ack is a `u64` seq through the session's condvar; `recv_acks` blocks
  the ack task until seqs exist. JS recycles removed ids only after
  their txn's seq acks. No sockets, no shared memory, no polling.
- Commit copy measured: 0.01 ms for a 683 KB transaction. A ring buffer
  would save nothing measurable.
- E2E verified: `bun examples/js/host.ts` runs React in a worker,
  submits through `craie-node` (N-API), wakes the winit loop, applies,
  renders, presents, acks.

## Verification so far

- `cargo test`: 17 lib tests (host, wire, ui, layout) + 1 cross-language
  fixture test.
- `bun test` in `packages/bridge`: 7 tests, 49 assertions (golden wire
  bytes, style mask round-trip, reconciler mount/update ops, ack-gated
  id recycling).
- `cargo run --example text -- --screenshot`: multilingual render incl.
  RTL Arabic, CJK, color emoji — verified visually.
- `cargo run --example app -- --screenshot`: wire-built UI rendered
  through the full apply -> Taffy -> Parley -> GPU path. Ordered paint
  verified (code background under monospace text).
- Ack round-trip verified through the session: `Root.flush()` resolves
  only after the native side applies the transaction.
- Idle is by construction (`Wait` + redraw only on request). The bridge
  wakes the loop only when a commit lands; between ticks nothing runs.
