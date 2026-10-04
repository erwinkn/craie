# Contract: lists (descriptor table, changes, viewport, callbacks)

Agreed 2026-10-04 by Craie A and Craie B through the coordinator, following
Erwin's direction: declarative data plus callbacks, and the same structure on
both sides. It builds on the list co-design study (contract C) and on
`callbacks.md`. Traces: `list-traces.md`.

## Shape

- **Data is a descriptor table.** Every item has a descriptor from the first
  paint: key, version, loaded, and an estimate. With it, layout (extents,
  scroll size, jump targets, anchoring) is computable before any row renders,
  on web, React Native and Craie alike.
- **Changes are ops against a revision:** `splice`, `move` and `update`. The
  kit's JS virtualizer and Craie's native one consume the same table and the
  same ops. Each runs its own copy of the algorithm, and both are held to the
  same JSON traces.
- **Callbacks are for lazy content and notification only:** load these rows,
  and the range or viewport changed. They are never for layout.
- **Rows stay ordinary React** (`renderItem(index)`), mounted for the range
  the virtualizer reports. Craie's React wrapper reshapes the table into wire
  records and sends it; native owns sizes, measurement and correction, so no
  sizes cross to JS.

## The table (JS)

```ts
type Estimate = number | { template: number; textLength?: number }  // textLength in Unicode scalars
interface ListItem {
  key: string
  version?: string | number   // changes when the row's layout may change; default 0
  loaded?: boolean            // default true; false: a placeholder row, content on demand
  estimate?: Estimate         // default 48
}
type EstimateTemplate =
  | { kind: "fixed"; size: number }
  | { kind: "widths"; bands: readonly (readonly [minWidth: number, size: number])[] }
  | {
      kind: "text"; base: number; inset: number; fontSize: number
      lineHeight: number      // logical points per line
      charWidth?: number      // mean advance as a fraction of fontSize; default 0.5
    }
type ListChange =
  | { kind: "splice"; at: number; remove: number; items: readonly ListItem[] }
  | { kind: "move"; from: number; count: number; to: number }     // `to` after removal
  | { kind: "update"; at: number; items: readonly ListItem[] }    // same keys, new version/loaded/estimate
interface ListSource {
  revision: number                       // increases for the list's life
  count: number
  get(index: number): ListItem           // pure, cheap: metadata, not message bodies
  indexOfKey?(key: string): number       // -1 when absent; else the kit scans get()
  changesSince?(revision: number): { base: number; revision: number; changes: readonly ListChange[] } | null
}
```

- **Edits.** A source that keeps a change journal makes edits O(k) on the
  wire. `changesSince` returning null (no journal, or history lost) means a
  full keyed reconciliation, so correctness never depends on seeing every
  render.
- **Arrays.** Plain arrays wrap into a source:
  `List<T>({ data, keyOf, versionOf?, describeItem? })`, as today. The
  wrapper diffs keys, and object identity is the default version.

## Props and handle (one API on both renderers)

```ts
interface VirtualListProps {
  source: ListSource
  templates?: readonly EstimateTemplate[]
  renderItem(index: number): ReactNode
  anchor?: "start" | "end"              // initial placement and follow mode
  anchorPolicy?: "reading" | "focus"    // default reading (study §3)
  endThreshold?: number                 // default 80
  followKey?: string | number
  overscan?: number                     // default 600
  startInset?: number; paddingEnd?: number; restoreKey?: string
  // callbacks (callback-ref slots on the list node)
  onRangeChange?(range: { first: number; last: number }): void   // mounted range, overscan included
  onViewportChange?(v: ListViewport): void                      // opt-in, coalesced per frame
  onAtEndChange?(atEnd: boolean): void
  loadItems?(range: { first: number; last: number }): void      // unloaded items near the viewport
}
interface ListHandle {
  scrollToIndex(i: number, o?: { align?: "start" | "center" | "end" }): void
  scrollToKey(key: string, o?: { align?: "start" | "center" | "end" }): void
  scrollToEnd(): void
  scrollToOffset(offset: number): void
  readViewport(): Promise<ListViewport | null>   // after preceding commits' layout; null once removed
}
interface ListViewport {
  revision: number
  range: { first: number; last: number }; visible: { first: number; last: number }
  anchor: { key: string; index: number; offset: number } | null
  pinnedKeys: readonly string[]          // rows kept mounted for focus
  offset: number; atEnd: boolean; following: boolean
}
```

- **Text templates in JS.** Web and React Native can't shape a sample, so a
  `text` template's estimate in JS is
  `base + lineHeight × max(1, ceil(textLength × fontSize × charWidth / (width − inset)))`.
  Native may keep its shaped-sample estimate. Traces never compare
  estimates, only what follows from given heights.
- **Callback slots and their delivery classes** (`callbacks.md`):

  | Slot | Arguments | Delivery |
  |---|---|---|
  | `loadItems` | `{ first, last }` | Reliable |
  | `onRangeChange` | `{ first, last }` | Every call |
  | `onAtEndChange` | `atEnd` | Every call |
  | `onViewportChange` | `ListViewport` | Latest wins (per frame) |

- **Lazy loading.**
  - Native asks for content only through `loadItems`, by the islet rule
    below.
  - JS answers by changing the data, not by returning a value. Once the
    content arrives, an `update` marks the items loaded with a new version
    and better estimates. Rows already mounted render the placeholder
    meanwhile.
  - So `loadItems` needs no reply channel. It is reliable (never dropped),
    and later calls supersede earlier ones.
- **Mount order.** `onRangeChange` drives which rows React mounts. The bridge
  mounts the reported range and pins focused rows, as Craie does today.

## Islets: no placeholder gap left on screen

An *islet* is a short placeholder run left on screen between loaded rows:
a gap the reader would see filling in piece by piece. A short loaded run
stranded between placeholders needs no rule of its own: the placeholder
runs on its two sides are judged separately, as below. The virtualizer prevents them through
the request it makes. It alone knows extents and the viewport height, so
it decides on both sides, and the source only answers.

- **Placeholder run.** A maximal run of consecutive `loaded: false` items.
- **Window.** The viewport, extended by `lookahead` logical points above
  and below. The default is one viewport height; traces set it explicitly.
- **Base request.** The unloaded items whose extents intersect the window.
  If there are none, there is no call.
- **Widening.** Take each side, above and below, where a placeholder run
  continues past the base request. If the part of the run past the
  request on that side has an estimated extent at most the viewport
  height, the request widens on that side to the run's end. Each side is
  judged alone. A run continuing past the request on both sides may widen
  on one side, both, or neither.
- **One request.** The request is one `{ first, last }` covering the base
  and every widening.
- **When to call.** After every change batch and every settled scroll, the
  request is computed again. `loadItems` is called when it is non-empty
  and differs from the last request made since the last change batch.
- **The source.** It loads the whole request, and may load more. It never
  loads less on purpose. A partial answer, an `update` that leaves part of
  the request unloaded, leads to the next request at the next evaluation.
- **At rest, no islet stays on screen.** Once the source has answered every
  request, no placeholder run intersects the viewport.
- **Anchor while loads are pending.** The topmost visible loaded row holds
  its place; with none loaded, the topmost visible placeholder holds.

Named traces I1–I4 (`harness/traces/lists/`):

- **I1.** Rows 0–399 and 420–999 loaded, rows 400–419 not, reader at row 380:
  the request covers 400–419 whole.
- **I2.** A jump into 50,000 unloaded rows, 10 rows short of a loaded
  region: the request joins that region. Far from any loaded region, it
  doesn't widen.
- **I3.** A partial answer leaves a 3-row placeholder run on screen: the next
  `loadItems` asks for exactly that run.
- **I4.** Loads pending above the reader: the topmost visible loaded row
  holds its place. **I4b:** with none loaded, the topmost visible
  placeholder holds.
- **I3j, I4j, I4bj.** The same data reached by a jump alone: an explicit
  jump holds through a partial answer and through loads landing above it.
  In I3, I4 and I4b, a reader scroll of 20 points after the jump makes the
  pending-load anchor rule decide.

## Behavior both sides implement (shared traces)

- **Anchoring (reading policy, default).**
  - For moves, the anchor stays when it stayed in order. Otherwise the
    topmost visible row that stayed holds its place (the kit's
    longest-increasing-subsequence rule).
  - For version or loaded changes, the topmost unchanged visible loaded row
    holds, then any unchanged visible row, then the first visible place.
  - An explicit jump wins until reader input; end-follow wins when active.
  - `focus` policy first holds a visible focused row that survived.
- **Measurement validity.** A measurement holds for (key, version, column
  width, template epoch); a move keeps it. An `update` invalidates the
  item's measurement, offscreen too: it takes the new estimate until laid
  out. On a width change, every measured offscreen row takes its estimate
  again, on both sides (the kit's web list now does this too).
- **Atomic batches.** A commit's ops apply as one batch against pre-change
  geometry. `followKey` and end jumps apply after the batch, before
  measurement correction.
- **Empty and out of range.** An empty range is `{first: 0, last: -1}`.
  Commands clamp indices; a missing key does nothing.
- **Traces.** Shared JSON step traces (`list-traces.md`) carry identities,
  versions, loaded state, viewport geometry, injected heights, and the
  expected anchor, offset, follow state, visible range and `loadItems`
  requests. TypeScript drives the kit core; Rust drives native `Ui`. They
  test what is observable, not equal estimates or equal overscan
  heuristics. Seeded traces back up the named ones (study §4 table).

## Wire (Craie; tags allocated by A)

| Tag | Name | Contents |
|---|---|---|
| 0x94 | `LIST_CONFIG2` | node, overscan f32, lookahead f32, fallback f32, template epoch u32, count u16, templates (kind u8, payload bytes u32, then fixed: size f32; widths: u16 count + f32 pairs; text: base, inset, font size, line height, char width f32) |
| 0x95 | `LIST_PATCH` | node, base revision u32, next revision u32, op count u32, then ops: splice (tag, at, remove, add count u32, descriptors), move (tag, from, count, to u32), update (tag, at, count u32, descriptors with the same item ids) |
| 0x96 | `LIST_ROW2` | row node, list node, item id, revision u32: a row measures for that item at that revision only, so a stale row never measures a new item |
| 0x97 | `LIST_POLICY` | scroller node, mode u8 (keep-visible, stick-to-end, none), anchor policy u8, end threshold f32, covered start f32, padding end f32 |
| 0x98 | `LIST_COMMAND` | list node, revision, request id u32, kind u8 (index, key, end, offset, read), then index or item id u32 + align u8, or offset f64 |
| 0x99 | `LIST_CACHE` | scroller, list, cache token u32, action u8 (attach, retain, release): warm restore by `restoreKey`, bounded natively |

- **Descriptor:** 16 bytes. Item id u32 (the bridge interns keys), version
  token u32, template id u16, flags u8 (loaded, numeric estimate), reserved
  u8, then an argument u32: the f32 estimate's bits, or the text length.
- **Costs.**
  - One update: 42 bytes. One move: 30 bytes. Prepending k rows: 30 + 16k
    bytes. A first mount of 50,000 rows: about 800 KB, once.
  - The 4 MiB session commit cap allows about 250,000 rows in one
    transaction. Past that, the bridge sends the first patch in pieces
    under one revision chain.
- **Revisions.** A patch with a stale base is rejected (a resync event, and
  JS resends a full reconciliation), not applied wrongly.
- **Events.**
  - `LIST_VIEWPORT`: list node, revision, mounted first/end, visible
    first/end, pinned ids, anchor id, index and offset, content offset (f64),
    flags (at end, following). It is coalesced per frame, and published for
    range, end and anchor changes; offsets are published only when
    subscribed.
  - Callback slots (`CALL`, callbacks note): `onRangeChange`,
    `onViewportChange`, `onAtEndChange` and `loadItems` on the list node.
- **Old ops.** `LIST_CONFIG`, `LIST_SPLICE` and `LIST_INDEX` (0x90–0x92) stay
  for current callers until they migrate; then they go.

## Native work (A), in order

1. 16-byte descriptors, patch validation, update and move with the
   revision-checked row identity; seeded patches equal a clean rebuild; wire
   bytes asserted.
2. The kit's anchoring rules, threshold and follow, jump commands,
   `readViewport`, and viewport events, all held to the shared traces.
3. A bounded warm cache (`LIST_CACHE`) with revision-safe restore.
4. The React wrapper (`VirtualList`, `ListContent` inside a ScrollView); the
   current `List` keeps working.

## Scope

- Removal animations stay out of the first version (DF-61), and `focus`
  policy stays opt-in, per the study's recommendations.
- B's kit review agreed: the kit's JS virtualizer consumes this table and
  these ops, with the changes folded in above (`indexOfKey`, text
  template metrics, `pinnedKeys`, delivery classes, islets).
