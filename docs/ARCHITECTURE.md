# Craie architecture

Craie is a prototype for a React-driven native desktop renderer. React is
authoritative for composition; native code retains only the state it needs
to produce pixels. The bet: owning the retained representation and the
renderer directly is a better fit for a React host than GPUI's retained
entity model, where the bridge had to translate React mutations into
GPUI's world.

The rule throughout: own exactly the state Craie needs, use specialized
libraries for the hard algorithms (Parley for text layout, Swash for
rasterization, etagere for atlas packing, Taffy for flex), and keep the
hot representation compact enough that its cost is obvious.

## Layers

```
React reconciler  (packages/bridge — JS, react-reconciler mutation mode)
       |  binary transactions submitted in-process (worker thread)
       v
crates/node       N-API: NativeClient.submit -> Session queue -> wake
                  subscribe(TSFN) <- acks + event frames
bridge.rs         bounded commit queue + ack/event outbox (Session)
wire.rs           flat op stream decode, validated, applied to the host
ui.rs             facade: apply -> layout -> paint; dispatch, focus, scroll
host/             retained tree: 20-byte node headers in a Vec<NodeHeader>
layout/           Taffy low-level traits over host storage + text measure
scene.rs          one ordered Vec<Instance> — quads and glyphs unified
text/             Parley layout -> Swash raster -> glyph cache -> atlas
input.rs          per-input editor state (Parley PlainEditor)
events.rs         normalized platform events + outbound frame encoding
clipboard.rs      arboard wrapper, UI-thread only
custom.rs         registered painters for CUSTOM nodes
a11y.rs           AccessKit tree projection + action channel
gpu/              wgpu: 1 instanced pipeline, growable buffer, atlas arrays
platform/         winit window, Wait control flow, AccessKit adapter
app.rs            the shared host app: session -> ui -> surface -> present
```

Data flows down. Nothing above `scene.rs` names wgpu; nothing above
`platform/` names winit.

## The wire (`wire.rs`, `packages/bridge/src/wire.ts`)

Modeled on gpui-react commit `46cb47d`. One React commit is one
transaction:

```
magic "CRW1" u32 | version u16 | flags u16 | seq u64 | string_count u32
strings: count x (u32 len + utf8)      -- referenced by index in ops
ops:     flat u8-tagged records to EOF
```

Ops: `create`, `set_text`, `text_props`, `set_style`, `place`, `detach`,
`remove`, `hidden`, `paint` (fill + corner radius + border), `props`
(listener mask + focusable), `input_props`, `custom` (tag + f32x4 +
text payload), `label` (accessibility name), `command` (focus, blur,
set-input-text, scroll-to), and `style` (a style *definition*: u64
presence mask + positional fields — display, position, flex-*, the
alignment keywords, gap, size/min/max, padding/margin/border/inset
rects, flex basis/grow/shrink, aspect ratio, overflow).

A transaction is validated fully before any op applies: referential
integrity, per-kind op legality, and decode errors reject the whole
commit, so a bad transaction can never half-mutate the host.

Decoding borrows strings from the buffer and produces typed ops with no
intermediate value objects. On the JS side, style objects are interned
by canonical JSON key so identical styles share one wire id and one
native `taffy::Style` record. The format and a JS fixture are checked by
`crates/craie/tests/wire_fixture.rs`.

## Retained host (`host/`)

`NodeHeader` is 20 bytes: a `parent` link, an `aux` row index into
per-kind side tables, a wire `style` id, a `kind` index (top bit =
hidden), a `generation` counter, and dirty flags. Ids are JS-assigned
and index the arena directly; free slots carry `EMPTY_KIND`;
`generation` bumps on reuse so stale references can never reach the new
occupant. Children live in a `Vec<NodeId>` side table parallel to the
arena (plus a `roots` list) — indexed access for Taffy, no sibling
links, empty vecs allocate nothing.

Dirty tracking is a queue: a LAYOUT transition pushes the node onto
`layout_dirty` once, and the layout pass drains it and walks ancestors —
no arena-wide flag scans.

Sparse per-kind data lives in side tables addressed by `aux`: `TextRow`
(text, font size, color) for `TEXT`, `ViewRow` (fill, radius, border)
for `VIEW`/`INPUT`/`CUSTOM`. Truly sparse state — listener masks,
focusability, scroll offsets, custom payloads, a11y labels — lives in
id-keyed maps on `Ui`, so the common path pays nothing for features a
node doesn't use.

## Layout (`layout/`)

Taffy 0.14 is used through its low-level traits (`TraversePartialTree`,
`LayoutPartialTree`, `CacheTree`, `RoundTree`,
`LayoutFlexboxContainer`) — it reads children and wire styles straight
out of the host and writes results into `Layouts`, a parallel per-node
store (cache, unrounded layout, final rect + content-box offset). There
is no second authoritative tree.

All of this is in **logical units**: styles, wrap widths, rects. The
display scale enters only at emit, where glyph origins and raster sizes
convert to physical pixels, so a monitor-scale change never reshapes.

Dirty propagation is explicit: a node's Taffy cache key covers only its
own inputs, so a mutation clears the cache on the node and every
ancestor (`invalidate`, driven by the host's dirty queue).
`clear_all_caches` covers style redefinition and resizes.

TEXT leaves are measured through Parley inside the leaf measure
callback; the resulting `Layout<Color>` is retained per node
(`MeasuredText`) and re-emitted at paint without reshaping unless the
text or its wrap width changed.

## Paint (`ui.rs`, `scene.rs`)

`Ui::render` computes layout for each root, then does a pre-order DFS
accumulating parent border-box origins (Taffy `location` is already
border-relative; adding content offsets again double-counts padding).
Scroll offsets shift children, not the scroller's own box. Nodes whose
border box misses the viewport emit nothing; children are still
visited. The scene is ONE `Vec<Instance>` in document order — painter
order is vector order — uploaded to a single growable buffer and drawn
in a single draw call.

`Instance` unifies everything the frame needs: rect fill, corner
radius, border, glyph runs, and a clip rect. Clipping is per-instance,
resolved by the walk's ancestor clip stack — no scissor passes, no
draw-call splits. Colors are authored sRGB decoded to linear at emit;
blending happens in linear and the surface is sRGB-encoded.

CUSTOM nodes get their `ViewRow` background plus a registered painter:
`Ui::register_painter(tag, fn)` receives the payload and the node's
logical rect and emits quads, scaled and clipped like everything else.
Painters registered from JS are invoked synchronously on the UI thread
during paint — no second framework, just a callback into the same
scene.

Each text node keeps an `EmittedText` batch: physical-pixel instances
keyed on scale, origin, and color. An unchanged repaint replays the
batch with zero shaping, rasterization, or glyph-cache lookups; a
color-only change rewrites instance colors in place.

## Text pipeline (`text/`)

Parley lays out a `ParagraphSpec` into a `Layout` that owns its shaped
data. Swash rasterizes only cache misses. The glyph cache keys on
interned font identity, interned coords/synthesis, glyph id, exact f32
size bits, and quarter-pixel subpixel buckets. The atlas is
etagere-packed 2048x2048 pages, R8 + RGBA, with CPU mirrors and dirty
rects; GPU upload writes only dirty regions. Atlas residency is
bounded: a page cap plus an LRU over glyph entries — when allocation
fails the least-recently-used allocations are deallocated and their
cache entries marked absent, so they re-rasterize on next use.

## Input, focus, scrolling (`ui.rs`, `input.rs`, `events.rs`)

`platform/` normalizes winit events into `Event`s; `Ui::dispatch` hit
tests through the layout tree (border boxes, ancestor clips, scroll
offsets) and walks the propagation path — every listener receives
pointer coordinates relative to its own border box, not the deepest
hit's. Pointer capture keeps a drag's move/up stream on the pressed
node. Wheel deltas scroll the nearest scrollable ancestor, clamped to
the style's scroll axes and the `content_size` extent.

Focus is native state: `Tab`/`Shift-Tab` traverse focusable nodes in
tree order, click focuses, `focus`/`blur` commands cross the wire, and
focus/blur/change events return to JS. INPUT nodes hold a Parley
`PlainEditor` each — caret, selection, undo/redo, clipboard (arboard),
and IME with the platform's cursor-area tracking.

Outbound events ride the same outbox as acks: `subscribe(TSFN)` hands
JS one callback for `ack | events` frames; pointer/key/text events
carry kind, target id, relative coords, and modifier bits.

## Accessibility (`a11y.rs`)

`martensite-accesskit-winit` (accesskit 0.25, patched for winit
0.31-beta.3) sits on the window. The semantic tree is projected from
retained state on demand: views become containers/buttons/scrollviews
(role follows listeners and overflow), text becomes labels carrying
their string, inputs become text fields with placeholder/value, hidden
and `display:none` subtrees are skipped. Bounds come from layout,
`accessibilityLabel` sets the name, and `clips_children` follows
overflow. The whole tree is republished when a11y-observable state
changes — small trees make full `TreeUpdate`s cheap. Actions (focus,
click, scroll, replace-text, scroll-into-view) queue on a shared
channel, wake the loop, and dispatch into `Ui` on the UI thread.

## GPU (`gpu/`)

One device, one instanced pipeline, one growable vertex buffer, atlas
texture arrays. One draw call per frame regardless of node count —
`Instance` unifies rects (`FLAG_SOLID`, no fetch) and glyphs (atlas
sample), so blending order follows instance order.

## Platform (`platform/`), bridge (`bridge.rs`), and node (`crates/node`)

winit 0.31 is confined to `platform/winit.rs`. The event loop runs
`ControlFlow::Wait`; frames are produced only in `RedrawRequested`.
`Wake` wraps `EventLoopProxy::wake_up` (payload-less in 0.31 — apps pair
it with their own channel, delivered to `App::woke`).

The runtime is one process: the main thread owns the winit event loop
(via `app.rs`'s `HostApp`), and React runs in a `worker_thread`.
`NativeClient.submit(bytes)` copies the encoded transaction into the
`Session`'s bounded commit queue and wakes the loop; `woke` drains,
applies each transaction atomically, acks its seq, and repaints once if
needed. The return channel is one N-API threadsafe function:
`NativeClient.subscribe(cb)` delivers `ack | events` frames as they are
posted, no polling task, no blocking `receive`. No sockets, no shared
memory — one copied commit per React transaction.

Acks close the loop for id ownership: JS holds removed ids until the
transaction that removed them is acknowledged, then recycles them.
Reuse can never race native apply, and `Root.flush()` resolves only
after the frame is confirmed.

## JS side (`packages/bridge`)

- `wire.ts`: byte-exact encoder mirror (writer, string interning,
  positional style encoding, style dedup).
- `host.ts`: JS-owned dense ids with a free list gated on native acks,
  prop diffing to minimal ops, `queueMicrotask` seal per commit.
- `index.ts`: react-reconciler 0.33 mutation-mode config; `<View>`,
  `<Text>`, `<TextInput>`, `<ScrollView>`, `<Custom>` components;
  `shouldSetTextContent` absorbs string children into the text prop.
- `native.ts`: `NativeTransport` over `NativeClient` (submit +
  `subscribe` frame handler), `runApp` (main thread: host + worker +
  event loop, `painters` option) and `attachApp` (worker side).
- `packages/react` (`@craie/react`): the public facade — typed
  components, event props (`onPointerDown`, `onFocus`, `onChangeText`,
  `onScroll`, …), `accessibilityLabel`, imperative commands
  (`focus`/`blur`/`scrollTo`/`setText`).

Apps bundle with esbuild and run under Node: Bun cannot `require`
N-API addons inside `worker_thread`s (upstream Bun bug), and Node
pipes worker `console` through the blocked main thread — `attachApp`
rebinds console to direct-fd writes so app logging works while the
event loop runs.

## What is deliberately not here

No animation, no multi-window, no `<Image>`. Per-subtree paint damage
and layout damage regions are not implemented — `paint` re-walks the
tree each repaint (emitting only viewport-visible instances) and the
root flex pass re-runs on any change (see EXPERIMENTS for what that
costs at 5,000 rows). The a11y tree republishes wholesale on change
rather than computing minimal `TreeUpdate`s.
