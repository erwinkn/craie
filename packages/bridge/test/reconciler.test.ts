import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, View, Text, ScrollView, Pressable, Bars, ROLE } from "../src/index.js"
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
