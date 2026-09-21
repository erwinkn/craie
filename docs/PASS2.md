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
viewport (no culling). Retained: 390 KiB headers, ~69 MiB heap (mostly
retained Parley layouts).

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
