# Shared list traces

One JSON file per trace, read by both list implementations: the kit's
TypeScript core (Craie B) and Craie's native virtualizer (Craie A, through
`Ui` and the list driver). A trace states data, geometry and the reader's
actions, then what must be observable after each step. It never states
estimates or overscan heuristics, which may differ between the two.

Files are named `<id>-<slug>.json`. Craie keeps its copy in
`harness/traces/lists/` and the kit in its own tree. Each side copies the
other's files verbatim, and the coordinator checks that both copies match.
The first eight (`I1` to `I4bj`) are the islet traces of `lists.md`;
`L1` to `L3` and `U1` to `U4` are its loading and unloading traces.

## A trace

```json
{
  "format": "craie-list-trace/1",
  "id": "I1",
  "name": "a short placeholder run inside the window loads whole",
  "viewport": { "width": 400, "height": 600 },
  "config": { "overscan": 600, "lookahead": 600, "endThreshold": 80,
              "anchor": "start", "anchorPolicy": "reading" },
  "templates": [],
  "items": [
    { "count": 400, "key": "m{i}", "loaded": true, "estimate": 40, "height": 40 },
    { "count": 20,  "key": "m{i}", "loaded": false, "estimate": 40 },
    { "count": 580, "key": "m{i}", "loaded": true, "estimate": 40, "height": 40 }
  ],
  "steps": [
    { "do": "mount" },
    { "do": "scrollToIndex", "index": 380, "align": "start",
      "expect": { "visible": [380, 394], "loadItems": [400, 419] } }
  ]
}
```

- **`viewport`.** The list's viewport, in logical points: the column width
  and the visible height. Scale 1, no insets, unless `config` sets
  `startInset` or `paddingEnd`.
- **`config`.** The list's props as the contract names them: `overscan`,
  `lookahead`, `retain`, `endThreshold`, `anchor`, `anchorPolicy`,
  `startInset` and `paddingEnd`. A trace that depends on one sets it; the
  rest take the contract's defaults.
- **`templates`.** Estimate templates, as in `lists.md`.
- **`items`.** Run-length blocks, in order:
  - `count` items each, with `key` a pattern where `{i}` is the item's
    index in the initial table;
  - `version` (default 0), `loaded` (default true), `failed` (default
    false), and `estimate`, a number or `{ "template": n, "textLength": k }`
    (default 48);
  - `height`, the height the row lays out at when mounted; it defaults to
    a numeric estimate, and is required with a template.

  Implementations use `height` as the measurement and never shape real
  content.

## Steps

Each step does one thing, then the virtualizer settles: layout, then
measurement corrections, then anchoring, then any `loadItems` call, then
any `unloadItems` calls (above, then below). Its `expect`, if present, is
checked after that.

| `do` | Fields | Meaning |
|---|---|---|
| `mount` | | First paint at offset 0, or at the end with `anchor: "end"` |
| `scroll` | `offset` | Reader input to an absolute content offset |
| `scrollBy` | `delta` | Reader input by a delta (positive: down) |
| `scrollToIndex` | `index`, `align` | The handle's jump |
| `scrollToKey` | `key`, `align` | The handle's jump by key |
| `scrollToEnd` | | The handle's jump to the end |
| `scrollToOffset` | `offset` | The handle's offset write, which counts as reader input |
| `changes` | `changes` | One commit's batch: `splice` / `move` / `update` as in `lists.md`; `items` are blocks as above, `{i}` counting from the op's `at` |
| `measure` | `key`, `height` | A mounted row's content laid out at a new height |
| `resize` | `width`, `height` | The viewport changes |
| `focus` / `blur` | `key` | Focus enters a row, or leaves it |
| `followKey` | `value` | The `followKey` prop changes |

## Expectations

All fields are optional. Offsets are logical points, compared within one
device pixel (1 at scale 1).

| Field | Meaning |
|---|---|
| `visible` | `[first, last]` of the items intersecting the viewport, exactly; `[0, -1]` when empty |
| `anchor` | `{ "key", "offset" }`: the anchor row and its top relative to the viewport's top, as the viewport event reports it |
| `offset` | The content offset of the viewport's top |
| `atEnd`, `following` | As in `ListViewport` |
| `pinnedKeys` | As in `ListViewport`, in any order |
| `mounted` | `{ "covers": [a, b] }`: the mounted range contains a..b (its exact extent is the implementation's) |
| `loadItems` | `[first, last]`: exactly one call since the previous step, with this request. `null`: no call. `{ "covers": [a, b] }`: one call whose request contains a..b |
| `unloadItems` | The ranges of the `unloadItems` calls since the previous step, in order: `[[a, b], ...]`; `[]` for none |
| `held` | `[first, last]` of the held rows (`ListViewport.held`), or `null` |

A step without `loadItems` (or `unloadItems`) in its `expect` doesn't
check those calls.
Implementations record calls in order. A trace that expects a call
consumes it.

## Running them

- **Craie** reads every file in `harness/traces/lists/` in a harness test
  (`list_traces.rs`). Mounted rows report the trace's heights, and
  `loadItems` and `unloadItems` calls are recorded from the list's
  callback slots.
- **The kit** reads its copy with its core and records the same.
- **Seeded traces** (generated changes, scrolls and loads) don't live in
  files. Each side checks its own against a clean rebuild, and only named
  traces are shared.
