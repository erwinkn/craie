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
- **The app passes the table as a plain array** (`items`), a new array
  for each edit. Edits reach the virtualizer as ops: `splice`, `move` and
  `update`. The app may pass them with the array as a fast path
  (`changes`); otherwise the list diffs keys. The kit's JS virtualizer and
  Craie's native one consume the same table and the same ops. Each runs its
  own copy of the algorithm, and both are held to the same JSON traces.
- **Two callbacks, for data and notification only:** `updateItems` (load
  these rows, these may go) and the optional `onVisibleChange`. They are
  never for layout.
- **Rows stay ordinary React,** from a render function passed as the
  list's child, mounted for a range the virtualizer keeps to itself: the
  kit renders its own on web and React Native, and on Craie native tells
  the bridge which rows to mount. Craie's React wrapper reshapes the table
  into wire records and sends it; native owns sizes, measurement and
  correction, so no sizes cross to JS.
- **The public surface** is plain props (`items`, `changes`, `templates`,
  `updateItems`, `onVisibleChange`, the row function as `children`) and
  the handle (`scrollToIndex`, `scrollToKey`, `scrollToEnd`,
  `scrollToOffset`, `readViewport`).

## The table (JS)

```ts
type Estimate = number | { template: number; textLength?: number }  // textLength in Unicode scalars
interface ListItem {
  key: string
  version?: string | number   // changes when the row's layout may change; default 0
  loaded?: boolean            // default true; false: a placeholder row, content on demand
  failed?: boolean            // with loaded false: the source gave up; shown as is, not asked again until the version changes
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
interface ListChanges {
  from: readonly ListItem[]   // the items array these ops edit, by identity
  ops: readonly ListChange[]  // applied in order, each against the result of the last
}
```

- **Items are immutable.** An edit is a new array; the list compares
  arrays by identity, so an array changed in place goes unseen. Rendering
  the same array again changes nothing.
- **Changes are a fast path.** When `items` is a new array and
  `changes.from` is the array of the last render, the list applies
  `changes.ops`, O(k) on the wire. Otherwise (no `changes`, or ones
  computed from another array, as after a skipped render) it diffs keys
  between the two arrays and derives the ops itself, O(n). Correctness
  never depends on `changes`; a store that already knows its edits (a
  journal, a streaming reply) saves the diff.
- **The keyed diff.** Removed keys are removed, new keys inserted, kept
  keys that changed order moved, and kept keys whose `version`, `loaded`,
  `failed` or `estimate` changed updated. Descriptor objects are compared
  by these fields, not by identity.
- **Keys.** `scrollToKey` and pinned rows need a key's index; the binding
  derives it from `items` (a map kept current through each batch). No
  `indexOfKey` prop.
- **No revision.** Revisions are internal: Craie's bridge numbers the
  patches it sends, and indices in `updateItems`, `onVisibleChange` and
  `readViewport` refer to the `items` of the last committed render.
- **Size.** A plain array of descriptor objects is fine at 50,000 rows:
  the diff is a key scan when there is no fast path, and a first mount
  sends the table once.

## Props and handle (one API on both renderers)

```ts
interface VirtualListProps {
  items: readonly ListItem[]
  changes?: ListChanges                 // the edits from the last render's items, as a fast path
  templates?: readonly EstimateTemplate[]
  children(index: number, row: { placeholder: boolean }): ReactNode  // placeholder: unloaded, or held (below)
  anchor?: "start" | "end"              // initial placement and follow mode
  anchorPolicy?: "reading" | "focus"    // default reading (study §3)
  endThreshold?: number                 // default 80
  followKey?: string | number
  overscan?: number                     // default 600
  lookahead?: number                    // default: one viewport height
  retain?: number                       // viewport heights kept loaded each side; default 3
  startInset?: number; paddingEnd?: number; restoreKey?: string
  // callbacks (callback-ref slots on the list node)
  updateItems?(ask: ListAsk): void          // data: load near the viewport, unload far from it
  onVisibleChange?(v: ListVisible): void    // optional; latest wins, at most once per frame
}
interface ListAsk {                         // what changed since the last ask
  load?: { first: number; last: number }               // at most one range; replaces the last
  unload?: readonly { first: number; last: number }[]  // at most two: far above, then far below
}
interface ListHandle {
  scrollToIndex(i: number, o?: { align?: "start" | "center" | "end" }): void
  scrollToKey(key: string, o?: { align?: "start" | "center" | "end" }): void
  scrollToEnd(): void
  scrollToOffset(offset: number): void
  readViewport(): Promise<ListViewport | null>   // after preceding commits' layout; null once removed
}
interface ListVisible {                     // what onVisibleChange reports
  first: number; last: number               // visible items; the Inbox's mark-seen reads these
  atEnd: boolean                            // "jump to latest" reads this
  following: boolean
}
interface ListViewport {                    // on demand, through readViewport
  visible: { first: number; last: number }
  anchor: { key: string; index: number; offset: number } | null
  pinnedKeys: readonly string[]             // rows kept mounted for focus
  offset: number                            // content offset of the viewport's top
  atEnd: boolean
  following: boolean
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
  | `updateItems` | `ListAsk` | Reliable; a newer `load` replaces an older one |
  | `onVisibleChange` | `ListVisible` | Latest wins, at most once per frame; nothing is sent without a listener |

- **Lazy content.**
  - The list asks for content, and offers to drop it, only through
    `updateItems`: loads by the islet rule, unloads by the retain window
    (below).
  - The app's data layer (the *source*, below) answers by rendering new
    `items`, not by returning a value. Once the content arrives, the new
    descriptors mark the items loaded with a new version and better
    estimates (an `update`, through `changes` or the diff); rows already
    mounted render the placeholder meanwhile. A source may also keep rows
    cached and change nothing.
  - So `updateItems` needs no reply channel. It is reliable (never
    dropped), and a newer `load` supersedes an older one.
- **Mount order.** The mounted range is internal. On Craie, native sends
  it to the bridge (`LIST_VIEWPORT`), which mounts it and pins focused
  rows, as Craie does today.

## Islets: no placeholder gap left on screen

An *islet* is a short placeholder run left on screen between loaded rows:
a gap the reader would see filling in piece by piece. A short loaded run
stranded between placeholders needs no rule of its own: the placeholder
runs on its two sides are judged separately, as below. The virtualizer prevents them through
the request it makes. It alone knows extents and the viewport height, so
it decides on both sides, and the source only answers.

- **Placeholder run.** A maximal run of consecutive `loaded: false` items
  that aren't `failed`. Below, *unloaded* means such an item.
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
- **At least a screen.** If the request's estimated extent (from its first
  row to its last) is under one viewport height, it extends one unloaded
  row at a time, before widening, until it reaches a viewport height. It
  extends in the reader's last scroll direction, or, after a mount or a
  jump, the way the placeholder run continues past the request (down if
  both ways). It stops where the run ends.
- **When to ask.** After every change batch, settled scroll, jump and
  resize, the list computes its ask again and calls `updateItems` with the
  difference from what it last asked, if any. Its `load` is the request,
  when it is non-empty and not contained in the last one asked since the
  last change batch: a smaller request inside a pending one would only cut
  it short. Without `load`, the last request stands.
- **The source.** It loads the whole request, and may load more. It never
  loads less on purpose. A partial answer, an `update` that leaves part of
  the request unloaded, leads to the next request at the next evaluation.
- **Failed rows** (`loaded: false, failed: true`) are never requested,
  until an `update` changes them.
- **At rest, no islet stays on screen.** Once the source has answered every
  request, no placeholder run intersects the viewport.
- **Anchor while loads are pending.** The topmost visible loaded row holds
  its place; with none loaded, the topmost visible placeholder holds. This
  holds at reader input too, not only when content lands: right after a
  scroll into placeholders, the anchor is the topmost visible loaded row
  (L1: m200, 100 points down).

Named traces I1–I4 (`packages/bridge/traces/lists/`):

- **I1.** Rows 0–399 and 420–999 loaded, rows 400–419 not, reader at row 380:
  the request covers 400–419 whole.
- **I2.** A jump into 50,000 unloaded rows, 10 rows short of a loaded
  region: the request joins that region. Far from any loaded region, it
  doesn't widen.
- **I3.** A partial answer leaves a 4-row placeholder run on screen: the next
  load starts at that run, extended to a screen and widened.
- **I4.** Loads pending above the reader: the topmost visible loaded row
  holds its place. **I4b:** with none loaded, the topmost visible
  placeholder holds.
- **I3j, I4j, I4bj.** The same data reached by a jump alone: an explicit
  jump holds through a partial answer and through loads landing above it.
  In I3, I4 and I4b, a reader scroll of 20 points after the jump makes the
  pending-load anchor rule decide.

## Loading without layout shifts

Erwin's rules (D135): at most one range load on screen, loads
of at least a screen, newer requests replacing older ones, and no layout
shifts when content lands. The islet rules above give the first three:

- **One placeholder run on screen.** The base request covers every
  unloaded row intersecting the viewport, as one range.
- **At least a screen,** in the scroll direction. Scrolling up half a
  screen into placeholders asks for the whole screen above (trace L1).
- **Newer requests replace older ones.** A `load` replaces the last at the
  source. An answer to an older request still applies, as data.

No layout shifts is the hold:

- **Held rows.** While a placeholder that isn't `failed` intersects the
  viewport, rows that load inside the viewport are *held*. They keep their
  estimated places, and `renderItem` gets `placeholder: true`. A held row
  isn't measured: its descriptor's version is laid out only on release.
  Traces name held rows from the first to the last.
- **Only arrivals hold.** A hold is for rows going from `loaded: false` to
  `true`. An `update` to a row already loaded (a streaming reply's new
  version) applies at once, even while a placeholder is visible, so a
  stream beside a skeleton never freezes.
- **Release.** When the last such placeholder loads or fails, every held
  row applies in that frame: native lays them out, measures them and
  anchors once, with the anchor rule.
- **Elsewhere.** Rows that load outside the viewport apply when their
  `update` arrives, and a held row that scrolls out of the viewport
  applies then.
- **Partial answers.** The rest of the visible part stays requested (the
  islet rule asks for it again), and the held rows wait.
- **No timeout.** A source that cannot load a row marks it `failed`. The
  hold then releases, and the failed row shows what `renderItem` gives a
  placeholder.
- **Jumps.** A jump into unloaded history shows placeholders at their
  estimated places until the visible part has arrived, then applies it in
  one frame. The jump target holds, as explicit jumps do (trace L2).

## Unloading

Erwin's rules (D136). The list decides what to unload, as it decides what
to load, and asks in the same `updateItems` call. The source answers with data. The descriptor table
stays the one source of truth, and the list never flips `loaded` itself.

- **Retain window.** The viewport extended by `retain` viewport heights
  above and below (default 3).
- **Visits.** A row is *visiting* from the first settled step where it
  intersects the retain window until a call names it. A row that never
  came near the reader, such as the rest of a long table loaded at mount,
  is never asked.
- **Candidates.** Loaded, visiting rows entirely outside the retain window
  extended by one more viewport height. That band keeps a row from being
  unloaded and reloaded while the reader scrolls back and forth. Pinned
  (focused) rows are never candidates, and neither are rows inside a
  pending load request (the last one, while one of its rows is still
  unloaded and not failed).
- **The ask.** `unload` holds at most one range per side, above first,
  then below, computed with the load. Each covers consecutive candidates,
  from the one nearest the viewport outward. A row that isn't a candidate
  ends it, and candidates past it wait for a later ask.
- **The source.**
  - It may drop the content: an `update` marks the rows `loaded: false`,
    with new versions.
  - Or it may keep them cached and change nothing. The ask ended their
    visit, so the list asks again only after they come back into the
    retain window and leave it again: an ask carries only what changed.

## Loading and unloading traces

- **L1.** A half-screen scroll up into placeholders asks for the whole
  screen above, then the answer lands taller in one frame, the reader's
  row holding.
- **L2.** A jump into unloaded rows; the visible content arrives in two
  parts and applies once, in one frame. Until then the loaded part is
  held.
- **L3.** As L2, but the rest of the visible part fails: the hold
  releases, and the failed rows show as placeholders and aren't asked
  again.
- **U1.** Scrolling far down unloads the far-above rows once; the source
  drops them.
- **U2.** Rows past the retain window but inside the band stay while the
  reader scrolls back and forth; one row past the band is asked.
- **U3.** A cached source ignores the calls: they aren't repeated until the
  rows come back and leave again.
- **U4.** A focused row is never unloaded: the range stops before it, the
  rows past it are asked next, and the row itself at the first ask after
  blur.

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
  expected anchor, offset, follow state, visible range and `updateItems`
  asks. TypeScript drives the kit core; Rust drives native `Ui`. They
  test what is observable, not equal estimates or equal overscan
  heuristics. Seeded traces back up the named ones (study §4 table).

## Wire (Craie; tags allocated by A)

| Tag | Name | Contents |
|---|---|---|
| 0x94 | `LIST_CONFIG2` | node, overscan f32, lookahead f32, retain f32 (viewport heights), fallback f32, template epoch u32, count u16, templates (kind u8, payload bytes u32, then fixed: size f32; widths: u16 count + f32 pairs; text: base, inset, font size, line height, char width f32) |
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
    first/end, held first/end, pinned ids, anchor id, index and offset,
    flags (at end, following). Coalesced per frame. The mounted range and
    holds are for the bridge (which rows to mount, which render as
    placeholders); the rest makes `onVisibleChange` (its four fields) and
    `readViewport`.
  - Callback slots (`CALL`, callbacks note): `updateItems` (a flags byte,
    bit 0 load, bits 1 and 2 an unload range above and below, then each
    present range as two u32) and `onVisibleChange` on the list node.
  - Descriptor flags gain `failed`.
- **Old ops.** `LIST_CONFIG`, `LIST_SPLICE` and `LIST_INDEX` (0x90–0x92) stay
  for current callers until they migrate; then they go.

## Native work (A), in order

1. 16-byte descriptors, patch validation, update and move with the
   revision-checked row identity; seeded patches equal a clean rebuild; wire
   bytes asserted.
2. The kit's anchoring rules, threshold and follow, jump commands,
   `readViewport`, and viewport events, all held to the shared traces;
   then loading with holds, and unloading (L and U traces).
3. A bounded warm cache (`LIST_CACHE`) with revision-safe restore.
4. The React wrapper (`VirtualList`, `ListContent` inside a ScrollView); the
   current `List` keeps working.

## Scope

- Removal animations stay out of the first version (DF-61), and `focus`
  policy stays opt-in, per the study's recommendations.
- B's kit review agreed: the kit's JS virtualizer consumes this table and
  these ops, with the changes folded in above (`indexOfKey`, text
  template metrics, `pinnedKeys`, delivery classes, islets).
- The props follow Erwin's call of 2026-10-04 (D149): plain props, with
  `items` as the table and an optional `changes` fast path, instead of a
  source object with a change journal, and rows from a render function
  passed as the child, which each binding converts for its backend.
- Loading and unloading follow Erwin's answers of 2026-10-04 (no layout
  shifts and at least a screen per load, D135; unloading with a retain
  window, D136), and his simplification to one data callback and one
  notification. They changed the expected requests of I3, I3j, I4 and I4j.
- The notification is `onVisibleChange` (Erwin, 2026-10-04): only the
  visible range and the end state are pushed; anchor, pinned keys and
  offset are read on demand with `readViewport`.
