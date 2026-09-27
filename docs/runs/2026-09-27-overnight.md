# Overnight run, 2026-09-27

An unattended implementation run on the `ARCHITECTURE-update.md` work
items, led by Claude Opus 5.5 in bb. Every PR is verified, reviewed by a
fresh Opus 5.5 thread (findings in `LEDGER.md`), fixed, and merged.

Host: exe1, a shared Linux VM (8 CPUs, no GPU: wgpu runs on llvmpipe,
software rendering). Other agents' browsers and dev servers kept its load
average between 9 and 15, so every time below is indicative and
labelled with where it ran. The macOS build and codesign steps cannot run
here; each PR says which checks ran.

## Timeline

Times are UTC.

| Time | What |
| --- | --- |
| 00:32 | `split` verified on exe1 (build, tests, bun, tsc, screenshots, smoke) |
| 00:46 | PR #1 (`split`) opened: Linux-host fixes to the release graph and font tests |
| 00:49 | PR #1 reviewed (5 findings, all fixed), merged as `f25f7b3` |
| 01:07 | E19 harness running; baseline and priority A/B on exe1 |
| 01:27 | PR #2 (E19) opened after verification (cargo, bun, tsc, smoke, windowed under Xvfb) |
| 01:30 | E15 walk baseline measured (1k, 10k, 100k nodes), in parallel with the review |
| 01:40 | PR #2 review fixed (9 of 10 findings); final E19 numbers rerun |
| 01:52 | E15 reach index built; oracle test and harness agree with the walk |
| 02:00 | E15 verification (cargo, wasm32, clippy, bun, tsc) |
| 02:32 | PR #3 (E15) review fixed (all 10 findings, plus a clamp panic in the rounded-clip hit test); E15 rerun twice |
| 02:36 | PR #3 reverified (CI, 282 workspace tests) and merged as `3e9f0f3` |
| 02:45 | Claims and keys (work item 1, protocol 4) built on E15; 9 dispatch tests plus 6 claim-rule unit tests, 10 bridge tests |
| 02:47 | PR #4 (claims) opened and sent to review. Two tracks start in parallel threads: sibling z (item 4) and runtime vectors with images (item 8) |
| 02:56 | State styles (item 5) track starts from the lead's design brief |
| 03:10 | PR #4 review fixed (13 findings, no blockers; `LEDGER.md` PR4-01..13, DF-12 for drop positions) |
| 03:15 | PR #4 reverified (CI, 300 workspace tests, smoke, macOS type-check) and merged as `a4e71a8`; tracks told to rebase onto it |
| 03:30 | Runtime vector shapes (item 8, part 1) pushed by its track; PR #5 opened and sent to review; images (part 2) continue stacked on it |
| 03:37 | Sibling z and layers (item 4, first half) pushed by its track; PR #6 opened and sent to review |
| 03:44 | PR #5 review: no blockers, 2 majors (a bad value in a drawing closed the session; work bounds per string, not per drawing), 7 minors; a new thread fixes them |
| 03:50 | PR #6 review: no blockers, 1 major (an owner layer closed when its own content left, leaving its nested menu unowned under it), 9 minors and nits; its track fixes them |
| 03:52 | State styles (item 5) pushed by its track; PR #7 opened and sent to review |
| 04:02 | Images (item 8, part 2) pushed by its track; PR #8 opened, stacked on #5, and sent to review |
| 04:15 | PR #6 review fixed (`LEDGER.md` PR6-01..10), reverified (CI, 311 workspace tests, smoke, macOS type-check) and merged |
| 04:18 | PR #7 review: no blockers, 2 majors (transitions ran on mount and on the first frame; layout values in a variant took their partner fields from the base, not from other variants), 13 minors; its track fixes them after merging main |
| 04:22 | PR #8 review: 1 blocker (decoding was not memory-bounded: a 249 KB PNG with a 16,000×16,000 header took 1.25 GB), 7 majors; its track fixes them |
| 04:35 | PR #5 review fixed (`LEDGER.md` PR5-01..13), main merged in, reverified (CI, 337 workspace tests, smoke, macOS type-check) and merged; PR #8 retargeted to main first |
| 04:47 | PR #7 review fixed (`LEDGER.md` PR7-01..22: first values snap, layout resolves per key, Suspense wins, disabled is fully disabled), main merged in twice, reverified (CI, 363 workspace tests, bun 80, smoke, macOS type-check) and merged |
| 04:52 | PR #8 review fixed (`LEDGER.md` PR8-01..17: decoding is bounded at 64 MP and 512 MiB before any buffer exists, the old image stays until the new one is ready, no dark edges, a 64 MB pixel budget, a panicking codec fails only its image), main merged in twice, reverified (CI, 395 workspace tests, smoke, macOS type-check). The blocker was about memory, so a fresh thread re-reviews the fix commit |
| 05:02 | PR #8 re-review: the blocker is closed; 1 new major (a 78-byte WebP whose EXIF chunk claims 4 GiB aborts the app where memory is committed up front), 4 minors |
| 05:10 | The intermittent `paths` crash characterised (below): the Vulkan validation layer, triggered by tests creating devices in parallel |
| 05:13 | PR #8 re-review fixed (`LEDGER.md` PR8-18..25), reverified (CI, 398 workspace tests, bun 87, smoke, macOS type-check) and merged |

## PRs

| PR | What | Status |
| --- | --- | --- |
| [#1](https://github.com/erwinkn/craie/pull/1) | The crate split, plus Linux-host test fixes | Merged |
| [#2](https://github.com/erwinkn/craie/pull/2) | E19: event round trip under load; React priorities for native events | Merged |
| [#3](https://github.com/erwinkn/craie/pull/3) | E15: hit-test reach index; propagation paths no longer allocate | Merged |
| [#4](https://github.com/erwinkn/craie/pull/4) | Claims and keys (work item 1): key records, keymaps, paste and drop claims; protocol 4 | Merged |
| [#5](https://github.com/erwinkn/craie/pull/5) | Runtime vector shapes (work item 8, part 1): SVG path strings parsed natively, dashes, a shared mesh cache | Merged |
| [#6](https://github.com/erwinkn/craie/pull/6) | Sibling z and layers (work item 4, first half): `zIndex` among siblings, layer containers that never sort below their owner | Merged |
| [#7](https://github.com/erwinkn/craie/pull/7) | State styles (work item 5): hover, press, focus, app states and breakpoints restyle natively; inherited color | Merged |
| [#8](https://github.com/erwinkn/craie/pull/8) | Images (work item 8, part 2): decoded off-thread at the drawn size, drawn from the glyph atlas | Merged |
| [#9](https://github.com/erwinkn/craie/pull/9) | GPU tests share one device per binary: fixes the intermittent `paths` crash | Merged |

## Numbers

Full tables in `docs/EXPERIMENTS.md`. All on exe1 (llvmpipe, loaded).

E19, click to drawn (GPU done), headless, ms (p50 / p95 / p99), and
the JS part alone (native dispatch to React's commit, p50 / p99):

| load | round trip | JS part |
| --- | --- | --- |
| idle | 11.3 / 20.0 / 27.0 | 0.40 / 4.0 |
| stream (60 tok/s into 400 messages) | 64.3 / 101.5 / 119.6 | 0.21 / 4.8 |
| gc (150 MB churning heap) | 16.1 / 37.6 / 70.0 | 0.64 / 35.2 |
| stream + gc | 80.0 / 135.2 / 178.8 | 0.84 / 50.8 |

JS is quick, garbage-collection pauses set its tail, and on exe1 the
rest is llvmpipe rasterizing. Rerun on the Mac: `sh bench/e19.sh`.

E15, µs per pointer move's hit test, the full walk against the reach
index (speedup):

| tree | 1k nodes | 10k | 100k |
| --- | --- | --- | --- |
| deep (cards of 40 nested views) | 13 → 0.12 (107x) | 131 → 0.82 (159x) | 1,259 → 5.3 (236x) |
| wide (groups of 5,000 cells) | 11 → 0.16 (70x) | 108 → 2.7 (39x) | 1,059 → 7.7 (137x) |
| list (one scroller of rows) | 5.6 → 0.48 (12x) | 66 → 4.6 (14x) | 694 → 50 (14x) |

Walk and index from the same run. Keeping the index costs 2 to 7
percent of the layout pass that stales it, and 16 bytes per node. Rerun on the Mac:
`cargo run --release -p craie-harness --example e15_lookups`.

The later work items, also on exe1 (CPU only for the core; medians,
loaded). Each PR has the full table and says what it means:

| What | Cost |
| --- | --- |
| Vectors: a new icon over a view (parse, tessellate) / from the cache | 11.4 µs / 3.2 µs |
| Vectors, worst case allowed (4,096 shapes × 1 KiB paths) | 7.4 ms to apply, 48 ms first frame |
| Sibling z: one z change among 5,000 siblings | 13 to 74 µs |
| Sibling z: the frame after a z change at 100k nodes | 6 to 9 ms (2.5 to 3.7 after a transform) |
| State styles: hover, 1 / 100 / 1,000 dependents | 0.27 / 11 / 116 µs, no allocation, no layout |
| State styles: breakpoint over 1,000 `_narrow` rows | 637 µs, of which about 130 µs is the restyle |
| Images: 4000×3000 JPEG to an 80×80 avatar | 75.5 ms off the main thread; 25.6 KB kept, not 48 MB |

## Decisions

- E19: native events' React updates take React DOM's priorities
  (discrete for press, key, focus, text; continuous for move, wheel,
  scroll). No measurable latency change; kept for the semantics.
- E19: the bridge renders a discrete event's updates before the next
  event of the same native batch (`flushSyncWork`), so a press sees the
  state the previous press left, as with the DOM's one task per event.
- E15: the hit test gets the index (a reach box per subtree, refreshed
  after each frame's layout and again before dispatch; a stale subtree
  is walked in full, and the reach is padded against rounding by the
  transform's condition number, so the index and the walk agree).
  Tab and the key walk with no focus stay walks: they are once per
  keypress and under 0.7 ms at 100k nodes, and claims (item 1) replace
  the key walk with a window list. Walking a propagation path no longer
  allocates.
- E15: the list's scroller still checks each row's reach in turn (14x,
  not 100x). Binary search over a column's sorted rows would fix it;
  not done, since long lists are virtualized.

- Claims and keys: see `ARCHITECTURE-update.md` topics 1 and 2,
  "Built". The ones to look at:
  - Claim sets travel in their own op, not the interaction op.
  - A paste is answered with `InsertText` (the focused input's
    selection when the answer lands), not a range against a revision;
    ranges come with rebased writes (topic 11, `LEDGER.md` DF-11).
  - All `useHotkeys` share one window list, the latest mounted hook
    first, and the first match wins: a dialog's Escape beats the
    page's. Marbre fires every hook that binds the chord.
  - Drops go to the focus path until the platform reports where they
    land (winit 0.30 does not; `LEDGER.md` DF-12).
  - `enter` submits on exactly Enter, as in Marbre.
  - The session now keeps acks and events in native order, so the
    facade drops old claim handlers exactly when native has moved on.
  - Escape no longer blurs; a key with nothing focused reaches no one
    (window shortcuts are claims); `mod+y` redoes (it undid).

- Tonight's wire changes all stay protocol 4: it is unreleased, so
  one bump covers the night. Each track owns a range of op tags, node
  kinds and event kinds, so the branches merge without renumbering:
  sibling z takes ops 0x22–0x2F and node kind 6, vectors and images
  0x72–0x7F, kind 7 and event 18, state styles 0xB0–0xBF.
- Tracks run as parallel threads on their own worktrees, stacked on
  the claims branch; each is rebased onto main, reviewed and merged
  as its own PR.
- Runtime vectors: shapes travel as SVG strings (path data, points,
  transforms, dash arrays) and native parses them with a small parser
  of its own, not usvg; the dash offset is not animatable yet (a
  spinner rotates its node; `LEDGER.md` DF-13), and `currentColor`
  waits for state styles (DF-14).
- Sibling z: `zIndex` rides the spatial op, so a z change never
  relayouts. A layer's sort key is (z, tree position, depth), each
  raised to its owner's, so "never below its owner" is a plain stable
  sort. A z change rebuilds the whole draw order (6 to 9 ms at 100k
  nodes against 2.5 to 3.7 ms after a transform; `LEDGER.md` DF-18);
  an incremental patch waits for a profile that asks for it.
- State styles: resolved natively, since the JS tail under GC (E19:
  35 to 50 ms p99) is too slow for hover. Specificity is the number of
  states a variant tests, then the highest-ranked state, then
  declaration order; a restyle sends only the fields that changed, so
  a transition tweens exactly those. Hover over 1,000 dependents
  restyles in 116 µs, with no allocation and no layout.
- Images: decoded by the `image` crate (png, jpeg, webp, gif) in
  `craie-platform-winit` only, on one worker thread, at the size drawn
  (an 80×80 avatar holds 25.6 KB, not the 48 MB of its 4000×3000
  source), and drawn as color quads from the glyph atlas: no new
  shader. Decoding is bounded before any buffer exists (64 megapixels,
  512 MiB), so a small hostile file can't claim gigabytes.
- A PR stacked on another is retargeted to `main` before its base
  merges: `--delete-branch` closes stacked PRs instead of retargeting
  them.
- macOS: exe1 cannot build or sign for the Mac, but
  `cargo check --workspace --target aarch64-apple-darwin` type-checks
  every `cfg(target_os = "macos")` path (no linking, no codesign). Each
  PR from #4 on runs it.

## Open questions for Erwin

- `_hover` on an element that is no scope (a Text, an Icon) means the
  nearest scope's hover in Craie; Marbre web means the element's own
  (`questions.tsx`, `tool-run.tsx`). Craie's rule is kept (DF-29). Align
  Marbre, or make such an element an implicit scope?
- Clippy has warnings in `craie-vector` and `craie-scene`, and
  `scripts/ci.sh` doesn't run it. Add clippy to CI, and fix them?
- The helper that characterised the `paths` crash installed `gdb` on
  exe1 with `sudo apt-get`. That's a change to a shared host; it
  reports installing nothing else.

## The `paths` crash

`craie-render --test paths` crashed (SIGSEGV) in PR #5's first CI run,
and failed once in PR #2's (that log wasn't kept, so it may be another
bug). A helper looped it on exe1 and read 13 core dumps:

| Configuration, 200 runs each | Crashes |
| --- | --- |
| Tests in parallel (the default) | 5 |
| `--test-threads=1` | 0 |
| `WGPU_VALIDATION=0` (no validation layer) | 0 |
| `LP_NUM_THREADS=1` (one llvmpipe thread) | 8 |
| One device shared by the binary's tests | 0 |

Every core's top frame is in the Khronos Vulkan validation layer
(1.3.275, Ubuntu 24.04), which wgpu loads in debug builds. Each test
made and dropped its own device. Most likely, while one thread was
inside the layer's `vkCreateDevice` or `vkDestroyDevice`, another's
call on its own device read freed layer state: one core catches a
create and a destroy in flight around the crash, but it isn't matched
to an upstream bug. It isn't Craie's rendering code, wgpu or llvmpipe.

The fix is in the tests (PR #9), and validation stays on:
- `paths`, `gpu_uploads` and `images` share one device per binary.
- `allocations` and platform-winit's own tests take a lock, so their
  devices never overlap. `allocations` measures allocations per phase,
  and a shared queue could finish the other test's work inside one.

After the fix, on exe1: `paths` 0 crashes in 200 runs, `gpu_uploads` 0
in 100, `images` 0 in 100. On the Mac, wgpu uses Metal, which has no
such layer.

## Next steps

On the Mac (exe1 can't link, sign or use a real GPU):
- Build, sign and run the app, then the pixel tests on a real GPU:
  `cargo test --workspace --no-fail-fast`.
- Rerun tonight's benchmarks. Every number above is from exe1, a
  loaded VM, with llvmpipe for anything the GPU does:
  - `sh bench/e19.sh`
  - `cargo run --release -p craie-harness --example e15_lookups`
  - `cargo run --release -p craie-harness --example vectors`
  - `cargo run --release -p craie-harness --example zorder`
  - `cargo run --release -p craie-harness --example states_restyle`
  - `cargo run --release -p craie-platform-winit --example images`
- Give touch and reduced motion a platform source (DF-25; the setters
  exist, with a TODO(macOS) in `app.rs`).

Work items not started:
- Item 3: focus traps, modals and inert content. This also covers Tab
  and accessibility order across layers (DF-17), and owners per
  element (DF-19).
- Item 6: transitions and animations inside variants (DF-22).
- The second half of item 4: anchor and geometry expressions.
- Topic 11: text ranges against a revision. This covers paste answers
  with a range (DF-11) and variants on nested Text (DF-26).

Deferrals to revisit when a profile asks for them:
- A z change rebuilds the whole draw order: 6 to 9 ms at 100k nodes
  (DF-18).
- No color management: Display-P3 photos look desaturated (DF-35).
- Large images share atlas pages that are never reclaimed (DF-36).
- JPEG DCT scaling, and more than one decoder thread (DF-33).
- An input's color, and vector `currentColor` reading the inherited
  `COLOR` (DF-14, DF-24).
