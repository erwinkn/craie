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
| 01:40 | E19 A/B done; branch verified (cargo, bun, tsc, smoke, windowed under Xvfb) |

## PRs

| PR | What | Status |
| --- | --- | --- |
| [#1](https://github.com/erwinkn/craie/pull/1) | The crate split, plus Linux-host test fixes | Merged |

## Numbers

Full tables in `docs/EXPERIMENTS.md`. All on exe1 (llvmpipe, loaded).

E19, click to drawn, headless, ms (p50 / p95 / p99):

| load | round trip | JS part, p50 / p99 (deliver + react, summed) |
| --- | --- | --- |
| idle | 15.6 / 34.0 / 44.2 | 0.4 / 7 |
| stream (60 tok/s into 400 messages) | 70.9 / 105.6 / 127.5 | 0.2 / 6 |
| gc (150 MB churning heap) | 12.9 / 27.2 / 64.8 | 0.5 / 37 |
| stream + gc | 72.7 / 117.8 / 156.7 | 0.7 / 47 |

JS is quick, garbage-collection pauses set its tail, and on exe1 the
rest is llvmpipe rasterizing. Rerun on the Mac: `sh bench/e19.sh`.

## Decisions

- E19: native events' React updates take React DOM's priorities
  (discrete for press, key, focus, text; continuous for move, wheel,
  scroll). No measurable latency change; kept for the semantics.

## Open questions for Erwin

## Next steps
