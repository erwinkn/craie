import { test, expect } from "bun:test"
import { Activity, createElement, useState } from "react"
import {
  createRoot, View, Text, TextInput, ScrollView, Pressable, Bars, List, ROLE, Vector, Image,
  Circle, G, Line, Path, Polygon, Rect,
} from "../src/index.js"
import { CraieHost } from "../src/host.js"
import { flattenShapes } from "../src/shapes.js"
import type { HostNode, Transport, UiEvent } from "../src/host.js"
import { readFrame } from "./crw2.js"
import { decodeEvents } from "../src/native.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  ackCb: ((seq: number) => void) | null = null
  eventCb: ((ev: UiEvent) => void) | null = null
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(cb: (seq: number) => void) { this.ackCb = cb }
  onEvent(cb: (ev: UiEvent) => void) { this.eventCb = cb }
  close() {}
  ops(i = -1) { return readFrame(this.frames.at(i)!).ops }
}

const tick = () => new Promise(r => setTimeout(r, 0))

test("render mounts a tree as wire ops", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(
    createElement(View, { backgroundColor: "#112233" },
      createElement(Text, { fontSize: 20, color: "#ffffff" }, "hello"))
  )
  await tick()
  const tags = t.ops(0).map(o => o.tag)
  expect(tags).toContain(0x01) // create
  expect(tags).toContain(0x02) // place
  expect(tags).toContain(0x30) // paint
  expect(tags).toContain(0x40) // paragraph
  const para = t.ops(0).find(o => o.tag === 0x40)!
  expect(para.s).toBe("hello")
  const span = readFrame(t.frames[0]!).spans[0]!
  expect(span.fontSize).toBe(20)
  expect(span.color).toBe(0xffffffff)
})

test("update emits only the changed op", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ n }: { n: number }) {
    return createElement(Text, { fontSize: 14 }, `count ${n}`)
  }
  root.renderSync(createElement(App, { n: 0 }))
  await tick()
  t.frames.length = 0
  root.renderSync(createElement(App, { n: 1 }))
  await tick()
  expect(t.frames.length).toBe(1)
  expect(t.ops().map(o => o.tag)).toEqual([0x40]) // paragraph only
})

test("transform and opacity travel in the spatial op, not layout", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ x }: { x: number }) {
    return createElement(View, {
      style: { width: 10, height: 10, transform: [{ translateX: x }], opacity: 0.5 },
    })
  }
  root.renderSync(createElement(App, { x: 0 }))
  await tick()
  t.frames.length = 0
  root.renderSync(createElement(App, { x: 5 }))
  await tick()
  const ops = t.ops()
  expect(ops.map(o => o.tag)).toEqual([0x20])
  // mask = transform only; matrix e = 5.
  expect(ops[0]!.f[0]).toBe(1)
  expect(ops[0]!.f[5]).toBe(5)
})

test("ids recycle without waiting for an ack; events carry generations", async () => {
  const t = new FakeTransport() // never acks
  const root = createRoot(t)
  const downs: string[] = []
  function App({ which }: { which: string }) {
    return createElement(View, null,
      createElement(View, {
        key: which,
        onPointerDown: () => downs.push(which),
      }))
  }
  root.renderSync(createElement(App, { which: "a" }))
  await tick()
  const idA = t.ops(0).filter(o => o.tag === 0x01).map(o => o.id)[1]!
  // React mounts the replacement before it releases the old node, so
  // "b" takes a fresh id and "a"'s id returns to the pool unacked.
  root.renderSync(createElement(App, { which: "b" }))
  await tick()
  expect(t.ops().some(o => o.tag === 0x04 && o.id === idA)).toBe(true)
  root.renderSync(createElement(App, { which: "c" }))
  await tick()
  expect(t.ops().some(o => o.tag === 0x01 && o.id === idA)).toBe(true)

  // An event for the old occupant (generation 0) is dropped; the new
  // occupant (generation 1) receives its own.
  const ev = (generation: number): UiEvent => ({
    kind: 2, node: idA, generation, revision: 0, x: 0, y: 0, a: 0, b: 0, key: 1 << 8, text: "",
  })
  t.eventCb!(ev(0))
  t.eventCb!(ev(1))
  expect(downs).toEqual(["c"])
})

test("an update in a press handler commits in microtasks, a timer's in a later task", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let bump = () => {}
  function App() {
    const [n, setN] = useState(0)
    bump = () => setN(n => n + 1)
    return createElement(View, { backgroundColor: n, onPointerDown: bump })
  }
  root.renderSync(createElement(App))
  await tick()
  const id = t.ops(0).find(o => o.tag === 0x01)!.id
  const microtasks = async () => { for (let i = 0; i < 10; i++) await Promise.resolve() }

  // Discrete (E19): React renders the sync lane in a microtask once the
  // event batch is dispatched, and the host seals in the next.
  t.frames.length = 0
  t.eventCb!({ kind: 2, node: id, generation: 0, revision: 0, x: 0, y: 0, a: 0, b: 0, key: 1 << 8, text: "" })
  await microtasks()
  expect(t.frames.length).toBe(1)

  // Outside an event, default priority: the scheduler renders it later.
  t.frames.length = 0
  bump()
  await microtasks()
  expect(t.frames.length).toBe(0)
  await tick()
  await tick()
  expect(t.frames.length).toBe(1)
})

test("each press in one event batch sees the state the previous one left", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let shown = false
  function Toggle() {
    const [open, setOpen] = useState(false)
    shown = open
    return createElement(View, { onPointerDown: () => setOpen(!open) })
  }
  root.renderSync(createElement(Toggle))
  await tick()
  const id = t.ops(0).find(o => o.tag === 0x01)!.id
  const press = { kind: 2, node: id, generation: 0, revision: 0, x: 0, y: 0, a: 0, b: 0, key: 1 << 8, text: "" }
  // Two presses delivered together, as native batches input while busy:
  // open, then closed again (not open twice from a stale `open`).
  t.eventCb!(press)
  t.eventCb!(press)
  await tick()
  expect(shown).toBe(false)
  t.eventCb!(press)
  await tick()
  expect(shown).toBe(true)
})

test("subtree deletion frees every node", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ show }: { show: boolean }) {
    return createElement(View, null,
      show ? createElement(View, null,
        createElement(Text, null, "a"),
        createElement(Text, null, "b")) : null)
  }
  root.renderSync(createElement(App, { show: true }))
  await tick()
  t.frames.length = 0
  root.renderSync(createElement(App, { show: false }))
  await tick()
  const tags = t.ops().map(o => o.tag)
  expect(tags.filter(x => x === 0x03).length).toBe(1) // detach the top
  expect(tags.filter(x => x === 0x04).length).toBe(3) // remove all three
})

test("facade components send explicit roles", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(
    createElement(View, null,
      createElement(Pressable, { onPress: () => {} }),
      createElement(ScrollView, null),
      createElement(Text, null, "x"),
      createElement(View, { onPointerDown: () => {} })),
  )
  await tick()
  const roles = t.ops(0).filter(o => o.tag === 0x50).map(o => o.f[0])
  // A plain View with listeners sends no role.
  expect(roles.sort()).toEqual([ROLE.button, ROLE.text, ROLE.scrollView].sort())
})

test("hidden sends display none through the layout op", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ hidden }: { hidden: boolean }) {
    return createElement(View, { hidden, style: { width: 10 } })
  }
  root.renderSync(createElement(App, { hidden: false }))
  await tick()
  t.frames.length = 0
  root.renderSync(createElement(App, { hidden: true }))
  await tick()
  const f = readFrame(t.frames.at(-1)!)
  expect(f.ops.map(o => o.tag)).toEqual([0x10])
  expect(f.styleCount).toBe(1)
})

test("bars surface sends kind, params, and payload bytes", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const values = new Float32Array([0.25, 0.75])
  root.renderSync(createElement(Bars, { values, color: "#ff0000", style: { width: 50, height: 20 } }))
  await tick()
  const ops = t.ops(0)
  const surface = ops.find(o => o.tag === 0x70)!
  expect(surface.f[0]).toBe(1) // SURFACE.bars
  expect(surface.f[1]).toBe(0xff0000ff)
  const payload = ops.find(o => o.tag === 0x71)!
  expect(new Float32Array(payload.bytes!.buffer)).toEqual(values)
})

test("TextInput is uncontrolled: value is initial only", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const { TextInput } = await import("../src/index.js")
  function App({ value }: { value: string }) {
    return createElement(TextInput, { value })
  }
  root.renderSync(createElement(App, { value: "A" }))
  await tick()
  const mount = t.ops(0).filter(o => o.tag === 0x80 && o.f[0] === 2)
  expect(mount.map(o => o.s)).toEqual(["A"])
  // Native edits produced "AB"; a later prop must not overwrite them.
  t.frames.length = 0
  root.renderSync(createElement(App, { value: "C" }))
  await tick()
  const later = t.frames.flatMap(f => readFrame(f).ops).filter(o => o.tag === 0x80)
  expect(later).toEqual([])
})

test("List sends config and items, renders the reported range, diffs splices", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  type Item = { id: number; text: string }
  const make = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => ({ id: from + i, text: `item ${from + i}` }))
  let items: Item[] = make(1000)
  const App = ({ items }: { items: Item[] }) =>
    createElement(ScrollView, { anchor: "stick-to-end" },
      createElement(List<Item>, {
        items,
        keyOf: (it) => it.id,
        describe: (it) => ({ template: 0, textLength: it.text.length }),
        templates: [{ base: 8, fontSize: 14 }],
        overscan: 300,
        initialCount: 5,
        renderItem: (it) => createElement(Text, null, it.text),
      }))
  root.renderSync(createElement(App, { items }))
  await tick()
  const ops = t.ops(0)
  const listId = ops.find(o => o.tag === 0x01 && o.f[0] === 4)!.id
  const config = ops.find(o => o.tag === 0x90)!
  expect(config.id).toBe(listId)
  expect(config.f).toEqual([300, 44, 8, 0, 14])
  const splice = ops.find(o => o.tag === 0x91)!
  expect(splice.f.slice(0, 3)).toEqual([0, 0, 1000])
  expect(splice.f.slice(3, 7)).toEqual([0, "item 0".length, 0, 0]) // template, length, id, flags
  expect(ops.find(o => o.tag === 0x93)!.f).toEqual([1]) // stick-to-end
  // Initial rows 0..5, each tagged with its item index.
  expect(ops.filter(o => o.tag === 0x92).map(o => o.f[0])).toEqual([0, 1, 2, 3, 4])

  // Native reports a range: React renders exactly those rows (and the
  // kept focused item).
  t.frames.length = 0
  // keep = item 900 (its id is 900: keys interned in order); revision 1.
  t.eventCb!({ kind: 14, node: listId, generation: 0, revision: 0, x: 900, y: 1, a: 500, b: 503, key: 900, text: "" })
  // The commit and React's deletion pass may seal separately: collect
  // every frame after both ran.
  for (let i = 0; i < 5; i++) await tick()
  const all = t.frames.flatMap(f => readFrame(f).ops)
  const indices = all.filter(o => o.tag === 0x92).map(o => o.f[0])
  expect(indices).toEqual([500, 501, 502, 900])
  expect(all.filter(o => o.tag === 0x04).length).toBe(5 * 2) // row views + texts

  // Append two items: one splice at the end, nothing else re-sent.
  t.frames.length = 0
  items = [...items, ...make(2, 1000)]
  root.renderSync(createElement(App, { items }))
  await tick()
  const appended = t.ops().filter(o => o.tag === 0x91)
  expect(appended.length).toBe(1)
  expect(appended[0]!.f.slice(0, 3)).toEqual([1000, 0, 2])
  expect(t.ops().filter(o => o.tag === 0x90).length).toBe(0)

  // Prepend three: one splice at 0; rendered rows move to new indices.
  t.frames.length = 0
  items = [...make(3, -3), ...items]
  root.renderSync(createElement(App, { items }))
  await tick()
  const pre = t.ops().filter(o => o.tag === 0x91)
  expect(pre.map(o => o.f.slice(0, 3))).toEqual([[0, 0, 3]])
})

test("List keeps the focused item's row across splices by identity", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  type Item = { id: number }
  const make = (from: number, n: number) => Array.from({ length: n }, (_, i) => ({ id: from + i }))
  let items: Item[] = make(0, 1000)
  const App = ({ items }: { items: Item[] }) =>
    createElement(ScrollView, null,
      createElement(List<Item>, {
        items,
        keyOf: (it) => it.id,
        initialCount: 0,
        renderItem: (it) =>
          createElement(TextInput, { value: `item ${it.id}`, accessibilityLabel: `in ${it.id}` }),
      }))
  const ops = () => t.frames.flatMap(f => readFrame(f).ops)
  const settle = async () => { for (let i = 0; i < 5; i++) await tick() }
  root.renderSync(createElement(App, { items }))
  await settle()
  const listId = ops().find(o => o.tag === 0x01 && o.f[0] === 4)!.id
  let revision = 1
  const range = (a: number, b: number, keepIndex: number, keepId: number, y = revision) =>
    t.eventCb!({ kind: 14, node: listId, generation: 0, revision: 0, x: keepIndex, y, a, b, key: keepId, text: "" })

  // Native: rows 500..503 visible, item 900 (id 900) focused.
  t.frames.length = 0
  range(500, 503, 900, 900)
  await settle()
  const rowOf = (index: number) => ops().filter(o => o.tag === 0x92 && o.f[0] === index).at(-1)!.id
  const row = rowOf(900)
  const input = ops().find(o => o.tag === 0x02 && o.f[0] === row)!.id
  const survives = (what: string, index: number) => {
    const all = ops()
    expect(all.some(o => o.tag === 0x04 && (o.id === row || o.id === input))).toBe(false)
    expect(all.some(o => o.tag === 0x01 && (o.id === row || o.id === input))).toBe(false)
    expect(all.filter(o => o.tag === 0x92 && o.id === row).map(o => o.f[0])).toEqual([index])
    void what
  }

  // Prepend three items: the focused row moves to index 903.
  t.frames.length = 0
  items = [...make(-3, 3), ...items]
  root.renderSync(createElement(App, { items }))
  await settle()
  revision++
  survives("prepend", 903)
  // A late event from the old item order: its indices are dropped, its
  // kept identity still holds (no rows for 600..603 appear).
  t.frames.length = 0
  range(600, 603, 900, 900, revision - 1)
  await settle()
  expect(ops().some(o => o.tag === 0x92 && o.f[0] === 600)).toBe(false)
  expect(ops().some(o => o.tag === 0x04 && o.id === row)).toBe(false)

  // Remove ten items before it: index 893.
  t.frames.length = 0
  items = items.slice(10)
  root.renderSync(createElement(App, { items }))
  await settle()
  revision++
  survives("removal before", 893)

  // Reorder: move it to the front.
  t.frames.length = 0
  const focused = items[893]!
  items = [focused, ...items.slice(0, 893), ...items.slice(894)]
  root.renderSync(createElement(App, { items }))
  await settle()
  revision++
  survives("reorder", 0)

  // Remove the focused item itself: its row goes.
  t.frames.length = 0
  items = items.slice(1)
  root.renderSync(createElement(App, { items }))
  await settle()
  expect(ops().some(o => o.tag === 0x04 && o.id === row)).toBe(true)
})

test("List marks moved items unchanged and edited items changed", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  type Item = { id: number; text: string }
  let items: Item[] = Array.from({ length: 6 }, (_, i) => ({ id: i, text: `item ${i}` }))
  const App = ({ items }: { items: Item[] }) =>
    createElement(ScrollView, null,
      createElement(List<Item>, {
        items,
        keyOf: (it) => it.id,
        describe: (it) => ({ textLength: it.text.length }),
        renderItem: (it) => createElement(Text, null, it.text),
      }))
  const splice = () => t.ops().filter(o => o.tag === 0x91).at(-1)!
  // (id, flags) per inserted item, after at, remove, count.
  const flags = () => { const f = splice().f; const out = []; for (let i = 3; i < f.length; i += 4) out.push([f[i + 2], f[i + 3]]); return out }
  root.renderSync(createElement(App, { items }))
  await tick()
  // Swap items 1 and 4: the same objects move.
  items = [items[0]!, items[4]!, items[2]!, items[3]!, items[1]!, items[5]!]
  root.renderSync(createElement(App, { items }))
  await tick()
  expect(splice().f.slice(0, 3)).toEqual([1, 4, 4])
  expect(flags()).toEqual([[4, 1], [2, 1], [3, 1], [1, 1]])
  // Edit item 2 in place: a new object with the same key and length.
  items = items.map(it => it.id === 2 ? { id: 2, text: "item X" } : it)
  root.renderSync(createElement(App, { items }))
  await tick()
  expect(splice().f.slice(0, 3)).toEqual([2, 1, 1])
  expect(flags()).toEqual([[2, 0]])
})

// Paragraph span rows of a frame's paragraph op on `id`.
function paragraphOf(t: FakeTransport, id: number, frame = -1) {
  const f = readFrame(t.frames.at(frame)!)
  const op = f.ops.find(o => o.tag === 0x40 && o.id === id)
  if (!op) return undefined
  const [start, count] = op.f as [number, number]
  return { text: op.s, spans: f.spans.slice(start, start + count) }
}

test("nested Text flattens to spans of one paragraph node", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ word, show }: { word: string; show: boolean }) {
    return createElement(Text, { fontSize: 16, color: "#ffffff", lineHeight: 22 },
      "Hé ",
      createElement(Text, { fontWeight: "bold", color: "#ff0000", fontFamily: "monospace" }, word),
      show ? createElement(Text, { textDecorationLine: "underline" }, " tail") : null,
      "!")
  }
  root.renderSync(createElement(App, { word: "wörld", show: true }))
  await tick()
  // One native text node: nested Text and string children are virtual.
  const creates = t.ops(0).filter(o => o.tag === 0x01)
  expect(creates.length).toBe(1)
  const id = creates[0]!.id
  const p = paragraphOf(t, id, 0)!
  expect(p.text).toBe("Hé wörld tail!")
  // Byte offsets: "Hé " is 4 bytes, "wörld" 6.
  expect(p.spans.map(s => s.start)).toEqual([0, 4, 10, 15])
  const [base, bold, under, bang] = p.spans
  expect([base!.fontSize, base!.weight, base!.lineHeight]).toEqual([16, 400, 22])
  expect([bold!.weight, bold!.color, bold!.family, bold!.fontSize]).toEqual([700, 0xff0000ff, "monospace", 16])
  expect([under!.decoration, under!.weight, under!.color]).toEqual([1, 400, 0xffffffff])
  expect([bang!.weight, bang!.decoration]).toEqual([400, 0])

  // A nested change resends only the paragraph.
  t.frames.length = 0
  root.renderSync(createElement(App, { word: "all", show: true }))
  await tick()
  expect(t.ops().map(o => o.tag)).toEqual([0x40])
  expect(paragraphOf(t, id)!.text).toBe("Hé all tail!")

  // Removing a nested Text recomposes; no native removal.
  t.frames.length = 0
  root.renderSync(createElement(App, { word: "all", show: false }))
  await tick()
  expect(t.ops().map(o => o.tag)).toEqual([0x40])
  expect(paragraphOf(t, id)!.text).toBe("Hé all!")
})

test("pointer events on a span reach its nested Text", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const hits: string[] = []
  root.renderSync(
    createElement(Text, { fontSize: 14, onPress: () => hits.push("outer") },
      "tap ",
      createElement(Text, { onPress: () => hits.push("link") }, "here"),
      createElement(Text, { fontWeight: "bold" }, " bold"))
  )
  await tick()
  const id = t.ops(0).find(o => o.tag === 0x01)!.id
  // The root's listener mask covers its nested Texts' listeners.
  const inter = t.ops(0).find(o => o.tag === 0x60 && o.id === id)!
  expect((inter.f[0]! & (1 << 2)) !== 0).toBe(true)
  const spans = paragraphOf(t, id, 0)!.spans
  expect(spans.length).toBe(3)
  // Revision 1: native applied one paragraph op.
  const up = (span: number, revision = 1): UiEvent => ({
    kind: 3, node: id, generation: 0, revision, x: 0, y: 0, a: 0, b: 0,
    key: (1 << 8) | ((span + 1) << 16), text: "",
  })
  t.eventCb!(up(1)) // "here": the nested link
  t.eventCb!(up(0)) // "tap ": the root
  t.eventCb!(up(2)) // " bold": no handler of its own, so the root
  expect(hits).toEqual(["link", "outer", "outer"])
})

// S3C-06: a nested Text replaced by an identical one (same text and
// style) still resends the paragraph, so the revision moves; an event
// hit-tested before native applied it goes to the root, not to the new
// owner.
test("a span event from an older span table reaches the root", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const hits: string[] = []
  function App({ k }: { k: string }) {
    return createElement(Text, { onPress: () => hits.push("outer") },
      "tap ",
      createElement(Text, { key: k, onPress: () => hits.push(k) }, "here"))
  }
  root.renderSync(createElement(App, { k: "old" }))
  await tick()
  const id = t.ops(0).find(o => o.tag === 0x01)!.id
  t.frames.length = 0
  root.renderSync(createElement(App, { k: "new" }))
  await tick()
  const all = t.frames.flatMap(f => readFrame(f).ops)
  expect(all.filter(o => o.tag === 0x40 && o.id === id).length).toBe(1)
  const up = (revision: number): UiEvent => ({
    kind: 3, node: id, generation: 0, revision, x: 0, y: 0, a: 0, b: 0,
    key: (1 << 8) | (2 << 16), text: "",
  })
  t.eventCb!(up(1)) // hit-tested before the resend: stale
  t.eventCb!(up(2)) // after it: the new owner
  expect(hits).toEqual(["outer", "new"])
})

test("selectable sets the interaction flag bit", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(
    createElement(View, { selectable: true },
      createElement(Text, null, "one"),
      createElement(Text, { selectable: true }, "two"))
  )
  await tick()
  const flags = t.ops(0).filter(o => o.tag === 0x60).map(o => o.f[1])
  // The View and the selectable Text send flag bit 1 (selectable).
  expect(flags.filter(f => f === 2).length).toBe(2)
})

// S3C-04: a nested text placed under a native parent becomes a native
// root with its own paragraph, and leaves its old root's; after root ->
// nested -> root, the new native node gets its paragraph again.
test("nested text moved to a native parent becomes a root", async () => {
  const t = new FakeTransport()
  const h = new CraieHost(t)
  const r = h.node("text", { children: "A" })
  const c = h.node("text", { children: "B" })
  h.place(null, r, null)
  h.place(r, c, null)
  await tick()
  const ops = () => t.frames.splice(0).flatMap(f => readFrame(f).ops)
  const texts = (list: ReturnType<typeof ops>) =>
    Object.fromEntries(list.filter(o => o.tag === 0x40).map(o => [o.id, o.s]))
  let o = ops()
  expect(o.filter(x => x.tag === 0x01).length).toBe(1)
  expect(texts(o)).toEqual({ [r.id]: "AB" })

  h.place(null, c, null) // nested -> root
  await tick()
  o = ops()
  expect(o.filter(x => x.tag === 0x01).map(x => x.id)).toEqual([c.id])
  expect(texts(o)).toEqual({ [r.id]: "A", [c.id]: "B" })

  h.place(r, c, null) // root -> nested: its native node goes
  await tick()
  o = ops()
  expect(o.some(x => x.tag === 0x04)).toBe(true) // remove
  expect(texts(o)).toEqual({ [r.id]: "AB" })

  h.place(null, c, null) // nested -> root again: sent fresh
  await tick()
  o = ops()
  expect(texts(o)).toEqual({ [r.id]: "A", [c.id]: "B" })
  expect(o.some(x => x.tag === 0x60 && x.id === c.id)).toBe(true)
})

// S3C-09: span starts are byte offsets of the text as encoded: a lone
// high surrogate is U+FFFD (3 bytes), and a pair split across pieces is
// one 4-byte character that stays in the span before.
test("span starts follow the encoded text around surrogates", async () => {
  const cases: [string[], number[]][] = [
    [["\uD800é", "Z"], [0, 5]],
    [["a\uD83D", "\uDE00b"], [0, 5]],
    [["a\uD83D", "\uDE00", "c"], [0, 5]],
  ]
  for (const [pieces, starts] of cases) {
    const t = new FakeTransport()
    const root = createRoot(t)
    const [first, ...rest] = pieces
    root.renderSync(
      createElement(Text, null, first,
        ...rest.map((p, i) => createElement(Text, { key: i, fontWeight: i % 2 ? "bold" : "normal", fontSize: 10 + i }, p))))
    await tick()
    const id = t.ops(0).find(o => o.tag === 0x01)!.id
    const p = paragraphOf(t, id, 0)!
    expect(p.spans.map(s => s.start)).toEqual(starts)
    const bytes = new TextEncoder().encode(pieces.join(""))
    for (const s of p.spans) {
      expect(s.start <= bytes.length).toBe(true)
      if (s.start < bytes.length) expect(bytes[s.start]! & 0xc0).not.toBe(0x80)
    }
  }
})

// S3C-13: a text root mounted hidden (Activity) sends its text; hiding
// is display: none natively, so revealing needs no paragraph. Updates
// while hidden reach it too.
test("a text root mounted hidden keeps its text", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ mode, word }: { mode: "hidden" | "visible"; word: string }) {
    return createElement(View, null,
      createElement(Activity, { mode, children: createElement(Text, null, "content ", word) }))
  }
  root.renderSync(createElement(App, { mode: "hidden", word: "one" }))
  await tick()
  const all = () => t.frames.splice(0).flatMap(f => readFrame(f).ops)
  let ops = all()
  const id = ops.find(o => o.tag === 0x40)!.id // the only text node
  const text = (list: typeof ops) => list.filter(o => o.tag === 0x40 && o.id === id).map(o => o.s).at(-1)
  expect(text(ops)).toBe("content one")
  root.renderSync(createElement(App, { mode: "hidden", word: "two" }))
  await tick()
  expect(text(all())).toBe("content two")
  root.renderSync(createElement(App, { mode: "visible", word: "two" }))
  await tick()
  ops = all()
  expect(text(ops)).toBe(undefined) // unchanged: not resent
  expect(ops.some(o => o.tag === 0x10 && o.id === id)).toBe(true) // layout: shown
})

// S3C-15: the revision does not repeat while an old event is pending:
// after 256 owner replacements, an event from the first table still
// reaches the root.
test("a span event survives 256 owner replacements as stale", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const hits: string[] = []
  function App({ k }: { k: number }) {
    return createElement(Text, { onPress: () => hits.push("outer") },
      "tap ",
      createElement(Text, { key: k, onPress: () => hits.push(`k${k}`) }, "here"))
  }
  root.renderSync(createElement(App, { k: 0 }))
  await tick()
  const id = t.ops(0).find(o => o.tag === 0x01)!.id
  for (let k = 1; k <= 256; k++) {
    root.renderSync(createElement(App, { k }))
    await tick()
  }
  const up = (revision: number): UiEvent => ({
    kind: 3, node: id, generation: 0, revision, x: 0, y: 0, a: 0, b: 0,
    key: (1 << 8) | (2 << 16), text: "",
  })
  t.eventCb!(up(1)) // from the first table
  t.eventCb!(up(257)) // current
  expect(hits).toEqual(["outer", "k256"])
})

// Event records are 36 bytes: the revision is a u32 after the key.
test("event records decode the 32-bit revision", () => {
  const buf = new Uint8Array(4 + 36 + 2)
  const v = new DataView(buf.buffer)
  v.setUint32(0, 1, true)
  v.setUint8(4, 3)
  v.setUint16(6, 9, true)
  v.setUint32(8, 42, true)
  v.setUint32(28, 7 << 16, true)
  v.setUint32(32, 0x0102_0304, true)
  v.setUint32(36, 2, true)
  buf.set([0x68, 0x69], 40)
  const [e] = decodeEvents(buf)
  expect([e!.kind, e!.generation, e!.node, e!.key, e!.revision, e!.text])
    .toEqual([3, 9, 42, 7 << 16, 0x0102_0304, "hi"])
})

// Step 4: style.transition travels in its own op: last at mount (first
// values do not tween), first on a change (it applies to the changes in
// the same commit); it never reaches the layout style.
test("transitions travel in their own op", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ o, fast }: { o: number; fast: boolean }) {
    return createElement(View, {
      style: {
        width: 10, opacity: o,
        transition: { opacity: { duration: fast ? 100 : 250, easing: "ease-out" }, width: { spring: { stiffness: 200 } } },
      },
    })
  }
  root.renderSync(createElement(App, { o: 1, fast: false }))
  await tick()
  let ops = t.ops(0)
  const tags = ops.map(o => o.tag)
  expect(tags.indexOf(0xa0)).toBeGreaterThan(tags.indexOf(0x10))
  const tr = ops.find(o => o.tag === 0xa0)!
  // count 2: opacity (1) then width (4), in property order.
  expect(tr.f[0]).toBe(2)
  expect(tr.f.slice(1, 9).map(v => Math.round(v * 1000) / 1000)).toEqual([1, 0, 0, 0.25, 0, 0, 0.58, 1])
  expect(tr.f.slice(9, 17).map(v => Math.round(v * 1000) / 1000)).toEqual([4, 1, 0, 200, 26, 1, 0, 0])
  // The layout style has no transition key: an unchanged transition
  // sends nothing; a changed one goes before the opacity it applies to.
  t.frames.length = 0
  root.renderSync(createElement(App, { o: 0.5, fast: false }))
  await tick()
  expect(t.ops().map(o => o.tag)).toEqual([0x20])
  t.frames.length = 0
  root.renderSync(createElement(App, { o: 0, fast: true }))
  await tick()
  expect(t.ops().map(o => o.tag)).toEqual([0xa0, 0x20])
})

// Step 4: node.animate encodes one property's target and timing.
test("animate sends one tween command", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let node: HostNode | null = null
  root.renderSync(createElement(View, { ref: (n: HostNode | null) => { node = n } }))
  await tick()
  t.frames.length = 0
  const n = node as unknown as HostNode
  n.animate("backgroundColor", "#ff0000", { duration: 300, easing: [0.1, 0.2, 0.3, 0.4], delay: 50 })
  n.animate("padding", 8, { spring: {} })
  n.animate("transform", [{ translateX: 5 }], { duration: 100 })
  await tick()
  const ops = t.ops()
  expect(ops.map(o => o.tag)).toEqual([0xa1, 0xa1, 0xa1])
  const r = (v: number) => Math.round(v * 1000) / 1000
  expect(ops[0]!.f.map(r)).toEqual([2, 0xff0000ff, 0, 0.05, 0.3, 0.1, 0.2, 0.3, 0.4])
  expect(ops[1]!.f.map(r)).toEqual([6, 8, 8, 8, 8, 1, 0, 170, 26, 1, 0, 0])
  expect(ops[2]!.f.slice(0, 7).map(r)).toEqual([0, 1, 0, 0, 1, 5, 0])
  expect(() => n.animate("gap", [1, 2, 3], { duration: 1 })).toThrow()
})

// DF-3 promoted into step 4: node.animate resolves from the native end
// event (per property, in call order); releasing the node resolves what
// is pending as removed; a node that is not mounted resolves cancelled.
test("animate resolves when native reports its end", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let node: HostNode | null = null
  function App({ show }: { show: boolean }) {
    return show ? createElement(View, { ref: (n: HostNode | null) => { node = n } }) : null
  }
  root.renderSync(createElement(App, { show: true }))
  await tick()
  const n = node as unknown as HostNode
  const id = t.ops(0).find(o => o.tag === 0x01)!.id
  const end = (prop: number, reason: number, generation = 0): UiEvent => ({
    kind: 15, node: id, generation, revision: 0, x: 0, y: 0, a: 0, b: 0,
    key: prop | (reason << 8), text: "",
  })
  const results: unknown[] = []
  const first = n.animate("opacity", 0, { duration: 100 }).then(r => results.push(["first", r]))
  const second = n.animate("opacity", 1, { duration: 100 }).then(r => results.push(["second", r]))
  const width = n.animate("width", 50, { duration: 100 }).then(r => results.push(["width", r]))
  t.eventCb!(end(1, 2)) // the first opacity tween: retargeted
  t.eventCb!(end(4, 0)) // width: finished
  t.eventCb!(end(1, 0)) // the second: finished
  await Promise.all([first, second, width])
  expect(results).toEqual([
    ["first", { finished: false, reason: "retargeted" }],
    ["width", { finished: true, reason: "finished" }],
    ["second", { finished: true, reason: "finished" }],
  ])
  // Pending at release: removed; the native event after it is dropped.
  const pending = n.animate("gap", 4, { duration: 100 })
  root.renderSync(createElement(App, { show: false }))
  await tick()
  expect(await pending).toEqual({ finished: false, reason: "removed" })
  t.eventCb!(end(7, 3))
  expect(await n.animate("opacity", 0, { duration: 1 })).toEqual({ finished: false, reason: "cancelled" })
})

// S4-10: a rejected animate call leaves nothing behind: no pending
// entry (a later call's end resolves that call) and no bytes in the
// transaction.
test("a rejected animate call writes nothing and holds nothing", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let node: HostNode | null = null
  root.renderSync(createElement(View, { ref: (n: HostNode | null) => { node = n } }))
  await tick()
  const n = node as unknown as HostNode
  const id = t.ops(0).find(o => o.tag === 0x01)!.id
  t.frames.length = 0
  await expect(n.animate("opacity", 0, { duration: 100, easing: "bad" as any })).rejects.toThrow()
  await expect(n.animate("opacity", 0, { duration: -1 })).rejects.toThrow()
  await expect(n.animate("gap", [1, 2, 3], { duration: 1 })).rejects.toThrow()
  await expect(n.animate("nope" as any, 1, { duration: 1 })).rejects.toThrow()
  expect(n.pendingAnims ?? []).toEqual([])
  const ok = n.animate("opacity", 0, { duration: 100 })
  await tick()
  expect(t.ops().map(o => o.tag)).toEqual([0xa1])
  t.eventCb!({
    kind: 15, node: id, generation: 0, revision: 0, x: 0, y: 0, a: 0, b: 0, key: 1, text: "",
  })
  expect(await ok).toEqual({ finished: true, reason: "finished" })
})

// Step 5b: a Vector node carries its asset as a payload (identity
// compare), and is an image for assistive technology by default.
test("vectors send their asset once per change", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const a = new Uint8Array([1, 2, 3])
  const b = new Uint8Array([4, 5])
  function App({ asset, bg }: { asset: Uint8Array; bg: string }) {
    return createElement(Vector, { asset, backgroundColor: bg, accessibilityLabel: "logo" })
  }
  root.renderSync(createElement(App, { asset: a, bg: "#000000" }))
  await tick()
  const ops = t.ops(0)
  const create = ops.find(o => o.tag === 0x01)!
  expect(create.f[0]).toBe(5)
  const payload = ops.find(o => o.tag === 0x71)!
  expect([...payload.bytes!]).toEqual([1, 2, 3])
  expect(ops.find(o => o.tag === 0x50)!.f[0]).toBe(ROLE.image)
  t.frames.length = 0
  root.renderSync(createElement(App, { asset: a, bg: "#ffffff" }))
  await tick()
  expect(t.ops().some(o => o.tag === 0x71)).toBe(false)
  t.frames.length = 0
  root.renderSync(createElement(App, { asset: b, bg: "#ffffff" }))
  await tick()
  expect([...t.ops().find(o => o.tag === 0x71)!.bytes!]).toEqual([4, 5])
})

// Work item 8: runtime shapes flatten into one DRAWING op, sent again
// only when the drawing changes. Vector props are inherited defaults.
test("vector shapes send one drawing per change", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ r, bg }: { r: number; bg: string }) {
    return createElement(Vector, {
      viewBox: "0 0 24 24", fill: "none", stroke: "#ffffff", strokeWidth: 2,
      strokeLinecap: "round", backgroundColor: bg,
    },
      createElement(Circle, { cx: 12, cy: 12, r, strokeDasharray: [4, 2] }),
      createElement(G, { transform: "translate(1 0)", opacity: 0.5, stroke: "#ff0000" },
        createElement(Path, { d: "m9 12 2 2 4-4", transform: "scale(2)" }),
        [createElement(Polygon, { key: "p", points: [0, 0, 4, 0, 2, 3], fill: "#00ff00", fillRule: "evenodd" })]),
      createElement(Rect, { width: 10, height: 6, rx: 2 }),
      createElement(Line, { x2: 5, y2: 5 }),
    )
  }
  root.renderSync(createElement(App, { r: 10, bg: "#000000" }))
  await tick()
  const ops = t.ops(0)
  expect(ops.find(o => o.tag === 0x01)!.f[0]).toBe(5)
  const d = ops.find(o => o.tag === 0x72)!
  expect(d.s).toBe("0 0 24 24")
  const [circle, path, poly, rect, line] = d.shapes!
  expect(circle!.strings).toEqual(["M22 12A10 10 0 1 1 2 12A10 10 0 1 1 22 12Z", "", "4 2"])
  // kind, rule, join, cap, fill, stroke, width, miter, offset, opacity
  expect(circle!.f).toEqual([0, 0, 0, 1, 0, 0xffffffff, 2, 4, 0, 1])
  expect(path!.strings).toEqual(["m9 12 2 2 4-4", "translate(1 0) scale(2)", ""])
  expect(path!.f[5]).toBe(0xff0000ff)
  expect(path!.f[9]).toBe(0.5)
  expect(poly!.strings[0]).toBe("0 0 4 0 2 3")
  expect(poly!.f.slice(0, 2)).toEqual([2, 1])
  expect(poly!.f[4]).toBe(0x00ff00ff)
  expect(rect!.strings[0]).toBe(
    "M2 0H8A2 2 0 0 1 10 2V4A2 2 0 0 1 8 6H2A2 2 0 0 1 0 4V2A2 2 0 0 1 2 0Z")
  expect(line!.strings[0]).toBe("M0 0L5 5")
  t.frames.length = 0
  root.renderSync(createElement(App, { r: 10, bg: "#ffffff" }))
  await tick()
  expect(t.ops().some(o => o.tag === 0x72)).toBe(false)
  t.frames.length = 0
  root.renderSync(createElement(App, { r: 8, bg: "#ffffff" }))
  await tick()
  expect(t.ops().find(o => o.tag === 0x72)!.shapes![0]!.strings[0]).toContain("A8 8")
})

test("vector shapes reject what they cannot draw", () => {
  const bad = [
    [createElement(Path, { d: "M0 0", fill: "currentColor" })],
    [createElement(View)],
    ["text"],
  ]
  for (const children of bad) expect(() => flattenShapes(children)).toThrow()
})

// Work item 8: an image sends its bytes once per change as a payload,
// its fit only when it is not the default, and native's image events
// reach onLoad and onError.
test("images send bytes once, fit on change, and report load and failure", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const bytes = new Uint8Array([1, 2, 3])
  const seen: unknown[] = []
  function App({ fit }: { fit?: "cover" | "contain" | "fill" }) {
    return createElement(Image, {
      src: bytes, fit, alt: "avatar",
      onLoad: e => seen.push(["load", e.width, e.height]),
      onError: e => seen.push(["error", e.message]),
    })
  }
  root.renderSync(createElement(App, {}))
  await tick()
  const ops = t.ops(0)
  const create = ops.find(o => o.tag === 0x01)!
  expect(create.f[0]).toBe(7)
  expect([...ops.find(o => o.tag === 0x71)!.bytes!]).toEqual([1, 2, 3])
  expect(ops.some(o => o.tag === 0x73)).toBe(false) // cover is native's default
  expect(ops.find(o => o.tag === 0x50)!.f[0]).toBe(ROLE.image)
  expect(ops.find(o => o.tag === 0x51)!.s).toBe("avatar")
  t.frames.length = 0
  root.renderSync(createElement(App, { fit: "contain" }))
  await tick()
  expect(t.ops().map(o => [o.tag, o.f[0]])).toEqual([[0x73, 1]])

  const ev = (key: number, text = "", generation = 0): UiEvent => ({
    kind: 18, node: create.id, generation, revision: 0, x: 40, y: 20, a: 0, b: 0, key, text,
  })
  t.eventCb!(ev(0))
  t.eventCb!(ev(1, "unsupported format"))
  t.eventCb!(ev(0, "", 1)) // a previous occupant's: dropped
  expect(seen).toEqual([["load", 40, 20], ["error", "unsupported format"]])
})

test("image URLs are fetched once per change; failures reach onError", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const errors: string[] = []
  const settle = async () => { for (let i = 0; i < 20; i++) await tick() }
  function App({ src }: { src: string }) {
    return createElement(Image, { src, onError: e => errors.push(e.message) })
  }
  root.renderSync(createElement(App, { src: "data:application/octet-stream;base64,AQID" }))
  await settle()
  const payloads = t.frames.flatMap(f => readFrame(f).ops).filter(o => o.tag === 0x71)
  expect(payloads.map(o => [...o.bytes!])).toEqual([[1, 2, 3]])
  t.frames.length = 0
  root.renderSync(createElement(App, { src: "/nonexistent/craie-image.png" }))
  await settle()
  // The old image must not stand in for the new src: empty bytes clear it.
  const sent = t.frames.flatMap(f => readFrame(f).ops).filter(o => o.tag === 0x71)
  expect(sent.map(o => o.bytes!.length)).toEqual([0])
  expect(errors.length).toBe(1)
  expect(errors[0]).toContain("ENOENT")
})

test("image URLs A, B, then A again send A's bytes again", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const settle = async () => { for (let i = 0; i < 20; i++) await tick() }
  const a = "data:application/octet-stream;base64,AQID"
  const b = "data:application/octet-stream;base64,BAU="
  const App = ({ src }: { src: string }) => createElement(Image, { src })
  for (const src of [a, b, a]) {
    root.renderSync(createElement(App, { src }))
    await settle()
  }
  const payloads = t.frames.flatMap(f => readFrame(f).ops).filter(o => o.tag === 0x71)
  expect(payloads.map(o => [...o.bytes!])).toEqual([[1, 2, 3], [4, 5], [1, 2, 3]])
})

test("unmounting an image cancels its fetch, without onError", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const errors: string[] = []
  const signals: AbortSignal[] = []
  const real = globalThis.fetch
  globalThis.fetch = ((_: unknown, init?: RequestInit) => new Promise((_, reject) => {
    const signal = init!.signal!
    signals.push(signal)
    signal.addEventListener("abort", () => reject(signal.reason))
  })) as typeof fetch
  try {
    function App({ show }: { show: boolean }) {
      return createElement(View, {}, show
        ? createElement(Image, { src: "https://example.com/a.png", onError: e => errors.push(e.message) })
        : null)
    }
    root.renderSync(createElement(App, { show: true }))
    for (let i = 0; i < 5; i++) await tick()
    expect(signals.length).toBe(1)
    expect(signals[0]!.aborted).toBe(false)
    root.renderSync(createElement(App, { show: false }))
    for (let i = 0; i < 5; i++) await tick()
    expect(signals[0]!.aborted).toBe(true)
    expect(errors).toEqual([])
  } finally {
    globalThis.fetch = real
  }
})

test("alt=\"\" marks an image decorative", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(createElement(Image, { src: new Uint8Array([1]), alt: "" }))
  await tick()
  expect(t.ops(0).some(o => o.tag === 0x50 || o.tag === 0x51)).toBe(false)
})
