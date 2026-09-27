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
| 02:45 | Claims and keys (work item 1, protocol 4) built on E15; 15 dispatch tests, 10 bridge tests |

## PRs

| PR | What | Status |
| --- | --- | --- |
| [#1](https://github.com/erwinkn/craie/pull/1) | The crate split, plus Linux-host test fixes | Merged |
| [#2](https://github.com/erwinkn/craie/pull/2) | E19: event round trip under load; React priorities for native events | Merged |
| [#3](https://github.com/erwinkn/craie/pull/3) | E15: hit-test reach index; propagation paths no longer allocate | Merged |

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
  - All `useHotkeys` share one window list: the first match in mount
    order wins, where Marbre fires every hook that binds the chord.
  - The session now keeps acks and events in native order, so the
    facade drops old claim handlers exactly when native has moved on.
  - Escape no longer blurs; a key with nothing focused reaches no one
    (window shortcuts are claims); `mod+y` redoes (it undid).

## Open questions for Erwin

- `craie-render --test paths` failed once in the first full test run
  of PR #2 and passed in five reruns. Likely a timing flake under
  llvmpipe load; worth watching on the Mac.

## Next steps
