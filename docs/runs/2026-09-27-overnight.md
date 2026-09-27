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
| 05:15 | PR #8 re-review fixed (`LEDGER.md` PR8-18..25), reverified (CI, 398 workspace tests, bun 87, smoke, macOS type-check) and merged |
| 05:18 | PR #9 opened: the GPU tests share a device; after it, `paths` crashed 0 times in 200 runs |
| 05:22 | PR #9 review: 2 majors (two more test binaries still opened devices in parallel), 4 minors and nits |
| 05:30 | PR #9 review fixed (`LEDGER.md` PR9-01..06: those tests take a lock), reverified (CI, 398 workspace tests, bun 87, smoke, macOS type-check) and merged. End of the run |

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
| [#10](https://github.com/erwinkn/craie/pull/10) | Morning: clippy in CI (`-D warnings`), every existing finding fixed or allowed with a reason | Merged |
| [#11](https://github.com/erwinkn/craie/pull/11) | Morning: a `Portal` or `Layer` starts a new scope chain; `TextInput` is a scope; load deflakes | Merged |
| [#12](https://github.com/erwinkn/craie/pull/12) | Morning: `currentColor` icons and inputs follow the inherited `COLOR` (DF-14, DF-24); protocol 5 | Merged |
| [#13](https://github.com/erwinkn/craie/pull/13) | Morning: this run log's follow-up section | Merged |
| [#14](https://github.com/erwinkn/craie/pull/14) | Afternoon: checked, expanded and selected reach assistive technology; switch, radio and radio group roles; protocol 6 | Merged |
| [#15](https://github.com/erwinkn/craie/pull/15) | Afternoon: the Mac run: allocation budgets per backend, E19 probe clock and timers fixed, Mac numbers | Merged |
| [#16](https://github.com/erwinkn/craie/pull/16) | Afternoon: this run log's afternoon follow-up | Merged |
| [#17](https://github.com/erwinkn/craie/pull/17) | Evening: presses and activation (work item 3, part 2): innermost press, one native activate, keep focus on press; protocol 7 | Merged |
| [#18](https://github.com/erwinkn/craie/pull/18) | Evening: focus traps, `modal` and `inert` (work item 3, part 1); layers owned by the trap they open from; protocol 8 | Merged |

## Numbers

Full tables in `docs/EXPERIMENTS.md`. On exe1 (llvmpipe, loaded) unless
marked Mac; the Mac's are in the follow-up's Numbers.

E19, click to drawn (GPU done), headless, ms (p50 / p95 / p99), and
the JS part alone (native dispatch to React's commit, p50 / p99):

| load | round trip | JS part |
| --- | --- | --- |
| idle | 11.3 / 20.0 / 27.0 | 0.40 / 4.0 |
| stream (60 tok/s into 400 messages) | 64.3 / 101.5 / 119.6 | 0.21 / 4.8 |
| gc (150 MB churning heap) | 16.1 / 37.6 / 70.0 | 0.64 / 35.2 |
| stream + gc | 80.0 / 135.2 / 178.8 | 0.84 / 50.8 |

JS is quick, garbage-collection pauses set its tail, and on exe1 the
rest is llvmpipe rasterizing. On the Mac the idle round trip is
1.01 / 1.19 / 1.24 ms (the follow-up's Numbers).

E15, µs per pointer move's hit test, the full walk against the reach
index (speedup):

| tree | 1k nodes | 10k | 100k |
| --- | --- | --- | --- |
| deep (cards of 40 nested views) | 13 → 0.12 (107x) | 131 → 0.82 (159x) | 1,259 → 5.3 (236x) |
| wide (groups of 5,000 cells) | 11 → 0.16 (70x) | 108 → 2.7 (39x) | 1,059 → 7.7 (137x) |
| list (one scroller of rows) | 5.6 → 0.48 (12x) | 66 → 4.6 (14x) | 694 → 50 (14x) |

Walk and index from the same run. Keeping the index costs 2 to 7
percent of the layout pass that stales it, and 16 bytes per node. On
the Mac, deep 100k is 642 → 4.1 (156x).

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
  one bump covers the night (the morning's #12 bumps to 5, and #14
  to 6). Each
  track owns a range of op tags, node kinds and event kinds, so the
  branches merge without renumbering:
  sibling z takes ops 0x22–0x2F and node kind 6, vectors and images
  0x72–0x7F, kind 7 and event 18, state styles 0xB0–0xBF.
- Tracks run as parallel threads on their own worktrees, stacked on
  the claims branch; each is rebased onto main, reviewed and merged
  as its own PR.
- Runtime vectors: shapes travel as SVG strings (path data, points,
  transforms, dash arrays) and native parses them with a small parser
  of its own, not usvg; the dash offset is not animatable yet (a
  spinner rotates its node; `LEDGER.md` DF-13), and `currentColor`
  waits for state styles (DF-14; done in #12).
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

Answered in the morning (details in the follow-up below):
- `_hover`: Marbre web also reads the nearest scope, so there is
  nothing to align; the reading below was wrong.
- Clippy: in CI since #10, every finding fixed or allowed.
- gdb on exe1: fine.

The questions as asked:

- `_hover` on an element that is no scope (a Text, an Icon) means the
  nearest scope's hover in Craie; Marbre web means the element's own
  (`questions.tsx`, `tool-run.tsx`). Craie's rule is kept (DF-29). Align
  Marbre, or make such an element an implicit scope?
- Clippy has warnings in `craie-vector` and `craie-scene`, and
  `scripts/ci.sh` doesn't run it. Add clippy to CI, and fix them?
- The helper that characterised the `paths` crash installed `gdb` on
  exe1 with `sudo apt-get`. That's a change to a shared host; it
  reports installing nothing else.

## Follow-up, morning and afternoon

Erwin read the run and asked, through the planning thread, for three
things: verify on the Mac, add clippy to CI, and settle the scope
questions. Erwin also told me directly that the protocol version may
be bumped when that makes things simpler or faster and no other
branch is in flight, and the same for anything held back to avoid
conflicts between parallel branches.

In the afternoon the MacBook came back: the Mac run landed (#15), and a
question from Marbre led to accessibility states (#14).

exe1's load was 35 to 70 most of the morning (Marbre's browser and
unit test suites, run by other threads), briefly 230, and 9 to 28 in
the afternoon. Times are UTC.

| Time | What |
| --- | --- |
| 06:29 | Mac thread starts on Erwin's MacBook, in its own worktree off `main` |
| ~06:30 | Clippy and inherited-color threads start on exe1 |
| 07:04 | The MacBook goes offline; the Mac thread has found the cause of both Mac test failures |
| 09:54 | PR #10 (clippy) and PR #11 (scopes) opened and sent to review |
| 10:14 | PR #12 (inherited color) opened and sent to review |
| 12:22 | PR #10 review fixed (`LEDGER.md` PR10-01..05) and merged |
| 13:33 | PR #11 review fixed (PR11-01..07), main merged in, reverified on the merged tree (CI with clippy, 398 Rust tests, 88 bun tests, smoke, macOS type-check) and merged |
| 13:39 | PR #12 review fixed (PR12-01..06), main merged in twice, reverified on the merged tree (CI with clippy, 404 Rust tests, 91 bun tests, smoke) and merged; its macOS type-check ran on the branch, before the last merge |
| 13:51 | PR #13 (this section) review fixed (PR13-01..08) and merged |
| 14:08 | The MacBook is back; the Mac thread resumes with main merged in |
| 14:49 | PR #14 (accessibility states) opened, from Marbre's question (Decisions, below) |
| 14:55 | PR #15 (the Mac run) opened by the Mac thread and sent to review |
| 15:05 | PR #14 review fixed (PR14-01..08), reverified (CI, 411 Rust tests, 93 bun tests, smoke, macOS type-check) and merged |
| 16:14 | PR #15 review fixed (PR15-01..06 on the Mac; PR15-07, Linux budgets, on exe1), main merged in, reverified on exe1 (CI, 411 Rust tests, 93 bun tests, smoke, macOS type-check) and merged |

### The Mac

The MacBook came back at 14:08. The Mac thread's findings are in #15;
it ran while Erwin was 3D modelling, so its numbers are pessimistic
and each carries its load average.

- **The two failing allocation tests had a stale budget, not a
  leak.** `list_frames_do_not_allocate` made 57 allocations against
  56, and `whole_frame_budgets` 127 against 125, both in
  `harness/invariants/tests/allocations.rs`. The budget for wgpu's
  share of a frame was 56 per frame plus 23 per extra pass, measured
  on wgpu 27 at one draw count. wgpu 30's cost has three parts:
  - a fixed cost per frame;
  - a cost per opacity layer (the layer's pass, its composite, and
    the parent's pass resumed after it);
  - each pass's command list: a `Vec` that starts at 4 and doubles.
    A pass records its draws plus 3 setup commands, and a list of up
    to 4 commands costs 1 allocation, 5 to 8 costs 2, 9 to 16 costs
    3, and so on.

  The fixed costs differ by backend:

  | backend | per frame | first layer | each layer, 1 to 5 layers |
  | --- | --- | --- | --- |
  | Metal (the Mac) | 53 | 67 | 62 to 70 |
  | Vulkan (exe1, llvmpipe) | 49 | 50 | 47 to 54 |

  For example, the opacity-0.5 frame has four passes: the parent
  before the layer, the layer, the composite, and the parent resumed.
  They record 5, 4, 3 and 10 commands, whose lists cost 2 + 1 + 1 +
  3 = 7. On Metal that's 53 + 67 + 7 = 127; the budget allows the
  most a layer costs, 70, so 130. Single-pass frames are exact on
  both machines. One extra allocation per frame, per pass or per draw
  fails both tests on both, and Craie's own phases still allocate
  nothing.

  #13's version of this section said each pass past 16 draws added
  one allocation (+1 on the list frame, +2 on the four-pass frame).
  That was half wrong. The list frame's +1 is its command list (16
  draws are 19 commands, past 16; the step is at 14 draws, not 16).
  But the four-pass frame draws only 11 times, and its +2 is wgpu's
  own work per pass.
- **The high E19 times were two bugs in the probe, not in Craie.**
  The Mac thread reproduced them in its own shell (an agent's);
  whether timers in a terminal Erwin opens wake as late wasn't
  measured. The CPU-only examples were never high.
  - Native and JS stamps read different clocks. The probe read
    `CLOCK_UPTIME_RAW`, which stops while the Mac sleeps; Node's
    doesn't. After 16.35 hours of sleep, every phase that joins the
    two sides was off by that much: idle "js" read 58,876,521 ms. The
    probe reads Node's clock now (`CLOCK_MONOTONIC_RAW`), and
    `report.py` exits with an error when any JS stamp falls outside
    native dispatch → apply.
  - The probe's own timers woke late. In the agent's shell, a 30 to
    70 ms sleep woke 37 to 82 ms late at the median and up to 150 ms
    (macOS coalesces timers), and neither QoS nor `mach_wait_until`
    helped. E19
    counted that as the click waiting for native. With the probe on,
    macOS now spins to each deadline. Linux timers weren't late, so
    Linux keeps its timed waits.

  Headless idle round trip p50 / p95 went from 23.85 / 124.67 ms to
  1.01 / 1.19 ms, and windowed from 34.85 / 74.85 ms to 0.29 /
  0.41 ms. The two end at
  different points: headless when the GPU finishes the frame,
  windowed when `present` returns. The display then adds up to one
  refresh, which E19 doesn't measure.
- **On the Mac, at f1fb16e** (before #14 was merged in, so 404 tests,
  not 411): `cargo test --workspace`, the pixel tests on Metal
  included, and `ci.sh` pass. The smoke test, with the ad-hoc-signed
  addon, passed earlier, at protocol 5. The merged tree was verified
  on exe1.
- **Marbre's side of the scope decisions is done.** Marbre already
  cut scopes at layers (marbre#22), and its spec now says so
  (marbre#27).

### Decisions

- **Protocols 5 and 6, each for a real change.** With no parallel
  branches left, the op tags could have been renumbered; each track's
  range already groups them by domain (0x2x spatial, 0x7x payloads,
  0xBx states), so they stay. #12 bumps the version because drawing
  shapes gained a byte (45 bytes; the new `current` byte marks a fill
  or stroke that follows the inherited color) and `INPUT_CONFIG` lost
  its color. The version check at load now compares the wire's own
  version, so a stale `craie-node.node` fails at load, not at its
  first frame. #14 bumps to 6 for the same reason (a byte added to an
  op; below).
- **Held back to avoid conflicts between parallel branches: one item,
  now done.** DF-24, vector `currentColor` and input color from the
  inherited `COLOR`, touched the vector and state-style tracks at once.
  #12 does it and closes DF-14 with it. A color change patches each
  shape's color record in place, as text colors already do: no new
  shader, no re-tessellation. The other variant values Marbre's kit
  uses (`z`, `pointerEvents`, visibility, elevation, the focus ring;
  DF-29) stay deferred because no screen the kit ports first needs
  them.
- **`_hover` on an element that isn't a scope** means the nearest
  scope's hover, in Craie as on Marbre web; the overnight question
  rested on a wrong reading of Marbre web. In
  `<Pressable><Text _hover={{ color: "red" }} /></Pressable>` the text
  turns red with the pointer anywhere on the Pressable, padding
  included.
- **A `Portal` or `Layer` starts a new scope chain** (#11), as on web.
  Native hover follows the native tree, where a layer's content isn't
  inside its opener. Before #11, a scope reached across the layer, so
  in
  `<Pressable expanded><Layer><Text _hover={{ color: "red" }}>Opus</Text></Layer></Pressable>`
  the menu item turned red with the pointer on the trigger. Now each
  item needs its own scope.
- **`TextInput` is its own scope** (#11). It takes no `disabled` yet:
  that would tell assistive technology the field is disabled while it
  still edits, and mask its hover and focus. It waits for a native
  read-only input. An unstyled input now takes its container's color
  (#12), as Marbre web's `.m-input` does with `color: inherit`.
- **Accessibility states reach assistive technology** (#14, protocol
  6). Marbre asked whether its `CheckInput` is a scope in Craie. It
  will be: its Craie adapter will build it on `Pressable`, which
  always is. But checking showed that only `disabled` reached
  AccessKit (the Rust library that talks to VoiceOver and AT-SPI): a
  checkbox had no checked state, and `expanded` and `selected` were
  never sent. Now
  `<Pressable accessibilityRole="checkbox" checked={on}>` is exposed
  as a checked or unchecked checkbox (VoiceOver would say "checkbox,
  unchecked", by AccessKit's mapping; no screen reader was tried),
  and `switch`, `radio` and `radiogroup` are roles. `expanded` and
  `selected` are reported only where the prop was given, so a plain
  button isn't announced as "collapsed" (on Windows and iOS; see
  Known gaps): the facade tells native which props were given (a
  byte on the ROLE op). `selected` is reported on list rows only, as
  Marbre web limits `aria-selected` to selectable roles: the kit also
  uses `selected` to style checkboxes and radios, which a screen
  reader would otherwise read as "checked, selected".
- **Clippy runs in `scripts/ci.sh`** with `-D warnings`, after the
  build (#10). 89 findings: 53 fixed; 34 under two workspace allows
  with reasons (`!(x > 0.0)` rejects NaN on purpose; taffy styles are
  built field by field); 2 `bad_bit_mask` allows on validity checks
  that can't fire while the mask uses all 8 bits, kept for when it
  widens. None was a bug. The toolchain is pinned, so new lints arrive
  only with a deliberate bump.
- **One cargo target directory per worktree, for parallel agent
  runs.** Cargo gives a workspace's crates the same hashes in every
  checkout, so two worktrees sharing a target directory overwrite each
  other's libraries mid-build: on exe1 a doctest failed with "can't
  find crate", and a macOS check of #12 compiled `craie-ui` against
  another checkout's `craie-vector`. `scripts/build-addon.sh` also
  copied the addon from `target/` whatever `CARGO_TARGET_DIR` said, so
  a smoke test could load another branch's addon; it follows
  `CARGO_TARGET_DIR` now. One checkout alone needs none of this.
- **Load flakes fixed** (#11). Bun tests that waited one tick for a
  React commit now wait for the op they check (`test/settle.ts`, at
  most 100 ticks): the text-root test and ten waits in
  `layers.test.ts`. `commit_events_go_out_without_a_frame`
  (platform-winit) now gives llvmpipe 30 s instead of 5 to draw its
  first frame. That loosens the bound, but the test only checks that
  the frame arrives, and run alone it takes 1.6 s.

### Open risks

- An unstyled `TextInput` inside a colored container used to draw
  white and now takes the container's color. That's intended, but a
  Marbre screen that relied on the white changes with no warning.
- CRV1, the vector asset format, gained paint kind 3 without a version
  bump. The change only adds a kind, and no reader outside this repo
  was found.
- `ci.sh` lints only the host's cfg. The macOS and wasm cfgs ran clean
  by hand (#10), and `ci.sh` ran clean on the Mac in #15 (at
  f1fb16e).
- During #10, one `cargo test --workspace` run timed out in
  `commit_events_go_out_without_a_frame`, and the binary then crashed
  (SIGSEGV) at exit. #11 fixed the timeout; the crash at exit is
  unexplained.

### Numbers

The Mac (M5 Max, release, AC power) against exe1, load average in
each cell (`docs/EXPERIMENTS.md` has every table):

| What | exe1 | Mac |
| --- | --- | --- |
| E19 headless idle, round trip p50 / p95 / p99 | 11.3 / 20.0 / 27.0 ms (load 13 to 15) | 1.01 / 1.19 / 1.24 ms (load 3.0 to 3.6) |
| E19 headless stream + gc | 80.0 / 135.2 / 178.8 ms (load 13 to 15) | 1.39 / 8.73 / 9.46 ms (load 3.0 to 3.6) |
| E19 windowed idle, to `present` | 7.8 / 16.3 / 21.5 ms (Xvfb, load 13 to 15) | 0.29 / 0.41 / 0.51 ms (load 9 to 12) |
| Hover, 1,000 dependents | 116 µs (load 10) | 74 µs (load 10 to 17) |
| Inherited color, 1,000 each: fill / icons / labels | 380 / 250 / 146 µs (load 36 to 55)¹ | 73.7 / 35.2 / 20.5 µs (load 10 to 17) |

¹ Medians of 14 runs, before a review fix (R12-04) removed a scan
over each drawing's items, so the icon number is an upper bound. The
load was high, so compare within the row: on exe1 the fill hover here
is 380 µs, against 116 at load 10 in the row above.

The E19 round trip is 11 to 58 times shorter at p50: on exe1 native
was the bottleneck (llvmpipe rasterizing), and on the Mac a click
rarely waits for it. Headless under stream, the Mac's p95 of about
9 ms is the loop's own 120 Hz pacing: the answer waits for the next
frame slot. Windowed looks faster than headless because it stops
earlier: at `present` returning, while headless waits for the GPU to
finish an 1800 × 1400 frame. The display adds up to one refresh after
`present`, which E19 doesn't measure. The CPU-only benchmarks ran 1.2 to 10 times as fast as on
exe1, mostly about twice, and their conclusions held. The PR #15
review reran E19 on exe1 with the Mac's spinning probe: within
exe1's usual spread, so the two tables compare.

## Evening: work items 3 and 6

The planning thread asked for the build-now items still open: work item
3 (focus traps, presses, focus groups), then work item 6 (motion). Item
3 runs as two parallel PRs, focus traps (#18) and presses (#17), with
the wire space split between them up front (flag bits and op tags; both
bump the protocol, and the second to merge takes the next number). Focus
groups follow, since they activate through #17's event. exe1's load was
11 to 26. Times are UTC.

| Time | What |
| --- | --- |
| 16:27 | Two threads start on item 3: focus traps, `modal` and `inert`; presses and activation |
| 17:06 | PR #17 (presses) opened and sent to review |
| 17:14 | PR #17 review: no blockers, 1 major (a link of two spans didn't activate when pressed on one and released on the other), 6 minors, 5 nits |
| 17:19 | PR #18 (focus traps) opened and sent to review |
| 17:30 | PR #17 review fixed (PR17-01..12), verified at its head, which is `main` plus the PR (CI, 424 Rust tests, 101 bun tests, smoke, macOS type-check) |
| 17:34 | PR #17 merged; #18 rebases onto it and takes protocol 8 |
| 17:37 | Transform parts (work item 6, part 1) starts |
| 17:39 | PR #18 review: no blockers, 2 majors (a `focus()` in the dialog's opening update became the restore target, so closing lost focus; a modal hidden by Suspense kept the app inert), 5 minors, 5 nits |
| 18:00 | PR #18 rebased on #17 and review fixed (PR18-01..09), verified at its head, which is `main` plus the PR (CI, 447 Rust tests, 105 bun tests, smoke, macOS type-check; hit tests still allocation-free) |
| 18:05 | PR #18 merged; focus groups (item 3, part 3) start |

### Decisions

- **Presses go to the innermost pressable, and `onPress` has one
  source.** In
  `<Pressable onPress={openThread}>… <Pressable onPress={archive}>Archive</Pressable></Pressable>`,
  clicking Archive archives and doesn't open the thread; the row's raw
  `onPointerUp` still fires. A click, Enter (on key down, with
  repeats), Space (on key up) and a screen reader's click all send one
  native `ACTIVATE` to the pressable, and `onPress` comes only from it,
  so the keyboard activates a button for the first time. A key claim
  on the same key wins. The screen reader's click no longer fakes a
  pointer press at the node's center, so an overlay drawn on top can't
  take it, and it doesn't turn the focus ring off.
- **A press is kept when the pointer leaves, and counts when it's
  released on the pressed node or inside it** (React Aria's rule). The
  web fires `click` on the common ancestor instead, so pressing Archive
  and releasing on the row opens the row there and nothing here
  (DF-45). A disabled pressable swallows its presses, and the row
  around it doesn't fire either.
- **`preventFocusOnPress`** keeps focus, caret and composition where
  they are, so a mention suggestion can be pressed while the composer
  keeps its caret.
- **`<Text onPress>` is a link.** Its role defaults to `link`, and a
  nested one is a pressable span: pressing "See" and releasing on
  "logs" in `<Text onPress={go}>See <Text weight={700}>logs</Text></Text>`
  activates it. Spans are pointer-only for now (DF-43), and a paragraph
  update mid-press cancels the press (DF-46).
- Deferred: `onLongPress` and `onMiddlePress` (DF-42); a keep-focus
  press hides the focus ring, where Chrome keeps it (DF-44).
- **Focus traps are native, and settle once per update.** In
  `<Layer z={70}><FocusTrap modal><View accessibilityRole="dialog">…</View></FocusTrap></Layer>`,
  opening focuses the `autoFocus` node (else the first focusable), Tab
  and Shift+Tab cycle inside, and closing returns focus to the node
  that had it before the update that opened the trap, if that node
  (id and generation) is still there. At the end of each update, focus
  moves at most once, to the last that applies: a closing trap's
  restore target, a `focus()` call, a new trap's auto-focus, an
  `autoFocus` node mounting into an active trap, then the rule for a
  removed focus (below). So closing a menu and its dialog together
  sends one blur and one focus.
- **`modal` makes the rest of the window inert** for the pointer, Tab
  and screen readers: the page and a toast under the dialog stop
  answering. The hit test starts only under the modal (no per-node
  ancestor checks; still no allocation). The most recently opened
  modal is on top, but never above a modal inside it. A modal hidden
  by Suspense is inactive until shown again. `inert` does the same for
  one subtree, and cancels a press in it.
- **A layer opened inside a trap is owned by it** (DF-19 closed): the
  dialog's menu stays live under the modal, and Tab reaches its items
  right after the button that opened it (DF-17's Tab half; screen
  readers still read layers last).
- **A removed focus goes to the trap's auto-focus target**, else its
  first focusable; outside traps, nowhere, now with a blur event (O1
  closed). Shift+Tab with nothing focused goes to the last focusable.
- Deferred: a `FocusTrap` is a layout box (DF-47); `autoFocus` outside
  a trap does nothing on mount (DF-48).

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
- A clean rerun when the Mac is quiet, windowed E19 included: this
  afternoon's ran at load average 3 to 17, beside Blender. The same
  commands:
  - `sh bench/e19.sh` (windowed, the window in front) and
    `CRAIE_HEADLESS=1 sh bench/e19.sh`
  - `cargo run --release -p craie-harness --example e15_lookups`
  - `cargo run --release -p craie-harness --example vectors`
  - `cargo run --release -p craie-harness --example zorder`
  - `cargo run --release -p craie-harness --example states_restyle`
  - `cargo run --release -p craie-platform-winit --example images`
- Whether a terminal Erwin opens delays timers as much as the agent's
  shell did (the second E19 bug).
- Time to photons: E19 stops at `present` returning, and the frame
  reaches the display up to one refresh later.
- Give touch and reduced motion a platform source (DF-25; the setters
  exist, with a TODO(macOS) in `app.rs`).

In Marbre:
- The Craie `CheckInput` adapter (`check-input.craie.ts`, only a
  declaration today) builds on `Pressable` and passes `checked` and
  `accessibilityRole`, so its checkboxes, switches and radios reach
  assistive technology.

Work items not started:
- The rest of item 3: focus groups (in progress).
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

Known gaps:
- An icon imported as CRV1 bakes `currentColor` at import (black by
  default), so it ignores its container's color (DF-39). Runtime
  shapes, which the kit's icons use, inherit it.
- `disabled` on `TextInput` waits for a native read-only input.
- `expanded` is in the accessibility tree, but the macOS and Linux
  AccessKit adapters don't read it yet, so a menu trigger is a plain
  button there (DF-40, upstream).
- `pressed`, a checkbox's mixed state, `highlighted` and the menu
  roles aren't reported yet (DF-41).
