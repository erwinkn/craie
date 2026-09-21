# Pass 2 — corrective architecture

Corrective pass on the first implementation. Good boundaries kept, bad
implementations deleted. This file records the baseline, then per-area
before/after and the final dependency decisions.

## Baseline (pre-pass)

- commit: `1a62b5c` (+ dirty working tree: occlusion handling, wgpu error
  hook, frame-dump debugging for the blank-window bug)
- machine: macOS Darwin 25.6.0, Apple M5 Max (arm64), 128 GB, Metal 4
- toolchain: rustc 1.97.1, wgpu 30.0.1, winit 0.31.0-beta.3, taffy 0.14.0,
  parley 0.11.1, swash 0.2.10, etagere 0.3.0
- `cargo test`: 16 pass (15 unit + JS wire fixture)
- `bun test` (packages/bridge): 7 pass
- headless screenshots: `docs/baseline/text.png`, `docs/baseline/app.png`
  (both correct — the bug is windowed present only)

### bench (release, 5,000 rows / 10,001 nodes, 1600x1000)

| phase                  | time     | allocs | live heap |
|------------------------|----------|--------|-----------|
| encode txn             | 11.8 ms  | 10,031 | 1.0 MiB   |
| decode + apply         | 0.48 ms  | 5,057  | 2.2 MiB   |
| layout+paint cold      | 484 ms   | 511k   | 52 MiB    |
| layout+paint "warm"    | 223 ms   | 5,003  | 61 MiB    |
| apply 500 set_text     | 0.02 ms  | 9      |           |
| layout+paint 500 upd   | 281 ms   | 52k    | 69 MiB    |

Scene: 5,001 quads + 222,780 glyph instances emitted for a ~1000px-tall
viewport (no culling). Retained state: 10,001 nodes x 40 B = 390 KiB
headers, ~69 MiB heap (mostly retained Parley layouts).

### structural sizes (baseline)

| type                 | bytes | per        |
|----------------------|-------|------------|
| NodeHeader           | 40    | node       |
| taffy::Cache         | 368   | node       |
| taffy::Layout        | 76    | node       |
| LayoutData           | 24    | node       |
| taffy::Style         | 240   | style      |
| parley::Layout       | 280 + heap | text node |
| GlyphKey / CachedGlyph | 12 / 16 | glyph  |
| QuadInstance         | 20    | quad       |
| GlyphInstance        | 40    | glyph      |

Per-node layout storage (cache + unrounded + rect) ≈ 468 B — dwarfs the
40 B header.

## What changed

### Presentation

`SurfaceTexture` in wgpu 30 presents on `Drop` — but the first-pass code
never even acquired frames correctly in a live window. Present is now
explicit: `queue.present(frame)` after `window.pre_present_notify()`,
with zero-size guards. Both examples verified rendering in real windows.

### Transport: TCP -> in-process session

Deleted: the TCP listener, the `client.ts` socket transport, the whole
port/handshake layer. A localhost socket for same-process commits was
the first pass's largest mismatch.

Now: `Session` (bounded queue, 256 txns / 4 MiB) + `Wake` on the winit
loop. `NativeClient.submit(bytes)` on the React worker copies one
`Vec<u8>` per commit and wakes the loop; the UI thread drains, decodes,
applies, and pushes seq acks through a condvar — `recv_acks` blocks, no
polling. JS recycles node ids only after their txn acks.

Measured: copying + draining a 683 KB commit is 0.01 ms. A shared-memory
ring would optimize a number that is already zero.

### Host storage: linked lists -> indexed arena

`NodeHeader`: 40 B -> 20 B. Gone: `first_child`, `next_sibling`,
`prev_sibling`, `child_count`, and the `PAINT` flag. Remaining:
`parent`, `aux`, `style` (wire id), `kind`+hidden, `generation`, dirty
flags.

Children moved to a `Vec<Vec<NodeId>>` side table indexed by node id
plus a `roots` list. Taffy's `child(index)` is now O(1); mutations still
allocate nothing for leaf nodes (empty `Vec`). `generation` still bumps
on slot reuse.

### Styles: no native interning

JS already dedupes styles and assigns dense wire ids. The wire id now
indexes `Layouts::styles` directly — the native HashMap-interning pass
is deleted.

### Coordinates: logical everywhere, physical at emit

Styles, Taffy, rects, and wrap widths are logical points. The display
scale enters only at `EmittedText` construction (glyph origins, raster
sizes) — so a monitor-scale change re-emits instances without
reshaping.

### Dirty: queue, not scan

A LAYOUT transition pushes the node onto `layout_dirty` once; layout
drains it and invalidates ancestors. No arena-wide flag walks. TEXT
dirt is separate: `set_text`/`set_font` dirty text (and layout), while
`set_color` dirties neither — color lives on the emitted instances.

### Text: paint-ready batches

Each text node retains `EmittedText`: physical-pixel `Instance` rows
keyed on (scale, origin, color). An unchanged repaint replays the batch
— zero shaping, rasterization, or glyph-cache lookups. Color change
rewrites instance colors in place. Swash scalers are created lazily;
coord interning is a small linear-scan table instead of per-run `Box`
allocations.

### Scene: ordered, unified, culled

`Vec<QuadInstance>` + `Vec<GlyphInstance>` (two draws, glyphs never
under quads) -> one `Vec<Instance>` in document order, one draw call.
`FLAG_SOLID` marks a rect; `FLAG_COLOR` a color glyph. Nodes outside
the viewport emit nothing — 10k nodes produce 1,101 instances.

## Final bench (release, same machine, same workload)

| phase                  | time     | allocs |
|------------------------|----------|--------|
| encode txn             | 11.3 ms  | 10,031 |
| decode + apply         | 0.54 ms  | 10,095 |
| layout cold            | 216.6 ms | 476k   |
| — compute / rounding   | 215.8 / 0.07 ms | |
| paint+emit cold        | 1.81 ms  | 263    |
| paint warm unchanged   | 0.04 ms  | 12     |
| layout warm unchanged  | 0.06 ms  | 0      |
| Taffy cache            | 10,001 hits / 75,001 misses | |
| apply 500 updates      | 0.02 ms  | 17     |
| layout 500 dirty       | 10.7 ms  | 44.5k  |
| paint 500 dirty        | 0.19 ms  | 33     |
| commit copy + drain    | 0.01 ms  | 3      |
| atlas upload           | 0.14 ms  | 47     |
| draw submit warm       | 0.18 ms  | 51     |
| GPU completion warm    | 0.70 ms  | 0      |

Retained: 10,001 x 20 B = 195 KiB headers; ~35 MiB live heap (mostly
Parley layouts — down from 69 MiB with culling + emitted-batch reuse).

Marbre-like transcript (2,000 messages, 10,501 nodes): mount applies in
0.54 ms, cold layout 125 ms, warm paint 0.03 ms. Streaming 300 appends:
0.645 ms layout + 0.031 ms paint per tick.

vs baseline: warm repaint 223 ms -> 0.04 ms; 500-update pass 281 ms ->
~11 ms; emitted instances 227k -> 1.1k; headers 390 -> 195 KiB; heap
69 -> 35 MiB.

## Decisions

| question | measured | decision |
|----------|----------|----------|
| transport | commit copy 0.01 ms / 683 KB | bounded queue; no ring, no TCP |
| Taffy cache | warm layout 0.06 ms; rounding 0.07 ms of 216 ms | keep `Cache` + `RoundTree` |
| Taffy itself | layout = 95%+ of every workload | keep for now; the lever is laying out less (virtualization), not a faster engine |
| Parley layouts | ~35 MiB retained for 5k–10k texts | keep shaping retained; window/distill only if a real workload exceeds memory |
| text repaint | warm paint 0.03–0.04 ms, ~10 allocs | `EmittedText` replay is the right level — no further distillation needed |
| atlas | 0.14 ms / 105 KB first upload; dirty rects steady | 2048² pages, etagere dirty-rect uploads: keep |
| wgpu | 0.18 ms submit + 0.70 ms completion warm | no abstraction cost to remove |
| NodeHeader | 20 B; 195 KiB for 10k nodes | done shrinking — 0.5% of live heap |

## Remaining limitations

- Layout is still the dominant cost and still visits every node on a
  relayout. A virtualized list / subtree damage is the next real lever —
  deferred until a product surface needs it.
- No events/hit-testing, scrolling, text input, animation,
  accessibility, multi-window, or `<Image>`.
- CJK line breaking is degraded without Parley's `icu` feature.
- `paint` re-walks the whole tree each repaint (cheap — 0.04 ms — but
  still O(nodes)); a damage region would make it O(visible).
