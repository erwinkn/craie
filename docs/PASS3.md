# Pass 3 — the interactive foundation

Pass 2 produced a correct render pipeline. Pass 3 makes it a foundation
an app can ship on: input, focus, text editing, scrolling, events back
to React, a public component API, custom native content, accessibility,
and bounded atlas memory.

## Baseline (pre-pass)

- commit: `864f019` (correctness checkpoint on top of the pass-2 tree)
- machine: macOS Darwin 25.6.0, Apple M5 Max (arm64), 128 GB, Metal 4
- toolchain: rustc 1.97.1, wgpu 30.0.1, winit 0.31.0-beta.3, taffy
  0.14.0, parley 0.11.1, swash 0.2.10, etagere 0.3.0, napi 3,
  react-reconciler 0.33, React 19.2, Node 26.3, Bun 1.4.2
- new deps: arboard 3.6.1 (clipboard), accesskit 0.25 +
  martensite-accesskit-winit 0.18.0 (adapter patched for winit
  0.31-beta.3; upstream accesskit_winit targets 0.30)

## What was added

### Input, focus, scrolling

winit events normalize into `Event`s at the platform edge. `Ui` hit
tests through border boxes, ancestor clips, and scroll offsets, then
walks the propagation path — each listener gets coordinates relative
to its own border box. Pointer capture keeps a drag on the pressed
node (the slider knob does not lose the stream). Wheel scrolls the
nearest scrollable ancestor, clamped to `overflow` axes and the Taffy
`content_size` extent. Focus traverses focusable nodes in tree order
and reports focus/blur/change to JS.

### TextInput

One Parley `PlainEditor` per INPUT node: caret, selection, undo/redo,
arboard clipboard, IME with live cursor-area tracking, placeholder,
`onChangeText`/`onSubmit`. Commands (`focus`, `blur`, `setText`,
`scrollTo`) cross the wire both directions.

### Return channel

The blocking `receive` N-API task is gone. `NativeClient.subscribe(cb)`
installs one threadsafe function that delivers `ack | events` frames —
acks, pointer/key/text/focus/scroll events, all on one channel. Two
real bugs surfaced here: `CalleeHandled` made the JS callback receive
`(error, frame)` (the "null frame" was the error arg), and a weak TSFN
did not keep the worker event loop alive. Both fixed; the TSFN is
strong and released on `Session::close`.

### Public API

`@craie/react` is the facade: typed `View`, `Text`, `TextInput`,
`ScrollView`, `Custom`; event props; `accessibilityLabel`; `focusable`;
imperative `HostNode` commands; `attachApp()` / `runApp(url, opts)`.
`@craie/bridge` stays the wire/reconciler machinery underneath.

### Custom elements

`NodeKind::CUSTOM` carries a tag, four f32s, and a text payload. Native
painters register per tag and emit logical-space quads into the same
clipped scene. JS painters register through `runApp(..., { painters })`
and are invoked synchronously on the UI thread. The widgets example
proves both extension modes: a slider built from ordinary `View`s and
pointer capture, and a sparkline painted by a registered callback.

### Accessibility

`a11y.rs` projects an AccessKit tree from retained state: roles follow
kind/listeners/overflow, bounds come from layout, labels from the
`accessibilityLabel` wire op, hidden subtrees drop out. The adapter
publishes full `TreeUpdate`s when a11y-observable state changes and
queues AT actions (focus, click, scroll, replace-text,
scroll-into-view) back onto the UI thread through the wake channel.

### Atlas residency

The glyph atlas is bounded: a page cap plus an LRU over cache entries.
Allocation failure evicts least-recently-used etagere allocations and
marks their cache entries absent; glyphs re-rasterize on next use.
Dirty-region upload is unchanged.

### Runtime: Node, not Bun

Bun cannot `require` N-API addons inside `worker_thread`s (upstream
bug; the require never returns). Examples now bundle with esbuild and
run under Node. Two consequences handled: worker `console` output is
invisible while winit blocks the main thread's event loop, so
`attachApp` rebinds console to direct-fd writes; and the copied addon
needs an ad-hoc re-sign on macOS (`scripts/build-addon.sh`) or dyld
kills the process with "Code Signature Invalid".

## bench (release, 5,000 rows / 10,001 nodes, same machine)

| phase                  | pass 2   | pass 3   |
|------------------------|----------|----------|
| decode + apply         | 0.48 ms  | 2.05 ms  |
| layout cold            | 484 ms*  | 567 ms   |
| paint+emit cold        | (folded) | 2.39 ms  |
| paint warm             | —        | 0.05 ms  |
| layout warm unchanged  | —        | 0.06 ms  |
| layout 500 dirty       | 281 ms*  | 19.30 ms |
| paint 500 dirty        | —        | 0.23 ms  |

*pass 2 numbers were layout+paint combined. Pass 3 splits them: paint
is now bounded by emitted instances (1,101 for the viewport), not node
count. The remaining cost is the Taffy cold pass (567 ms for 10k
nodes) — unchanged and still the thing a production host would attack
with subtree damage tracking.

## Verification

- `cargo test -p craie`: 29 lib tests + 1 cross-language fixture test —
  host, wire validation, layout, dispatch (hit-test through scroll +
  clip, focus, wheel), input editing, custom painters, a11y tree
  (roles, labels, bounds, focus, hidden pruning), atlas eviction and
  reallocation.
- `bun test` in `packages/bridge`: 7 tests, 49 assertions.
- `node examples/js/dist/smoke.mjs`: attach → submit → apply → TSFN
  ack → `flush()` resolves → clean close. PASS.
- `pnpm dev:widgets`: attaches, painter fires on the UI thread
  (8 quads into the scene), series ticks repaint.
- `pnpm dev:todo`: attaches, loads persisted items.
- macOS addon re-sign verified: fresh builds load without the code
  signature invalidation kill.

Not verified here: visual window output (no screen access in this
session) and a live screen-reader pass — the AccessKit pipeline is
exercised by unit tests and adapter wiring, not by VoiceOver.
