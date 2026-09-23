import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, View, Text, TextInput, ScrollView, Pressable, Bars, List, ROLE } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { readFrame } from "./crw2.js"

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
    kind: 2, node: idA, generation, x: 0, y: 0, a: 0, b: 0, key: 1 << 8, text: "",
  })
  t.eventCb!(ev(0))
  t.eventCb!(ev(1))
  expect(downs).toEqual(["c"])
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
  t.eventCb!({ kind: 14, node: listId, generation: 0, x: 900, y: 1, a: 500, b: 503, key: 900, text: "" })
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
    t.eventCb!({ kind: 14, node: listId, generation: 0, x: keepIndex, y, a, b, key: keepId, text: "" })

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
  const up = (span: number): UiEvent => ({
    kind: 3, node: id, generation: 0, x: 0, y: 0, a: 0, b: 0,
    key: (1 << 8) | ((span + 1) << 16), text: "",
  })
  t.eventCb!(up(1)) // "here": the nested link
  t.eventCb!(up(0)) // "tap ": the root
  t.eventCb!(up(2)) // " bold": no handler of its own, so the root
  expect(hits).toEqual(["link", "outer", "outer"])
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
