import { test, expect } from "bun:test"
import { createElement, Fragment, Suspense, useState } from "react"
import { createRoot, Layer, View } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { NIL } from "../src/wire.js"
import { readFrame, type Op } from "./crw2.js"
import { settle } from "./settle.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_cb: (seq: number) => void) {}
  onEvent(_cb: (ev: UiEvent) => void) {}
  close() {}
  ops(i = -1) { return readFrame(this.frames.at(i)!).ops }
  all() { return this.frames.flatMap(f => readFrame(f).ops) }
  /** Ops went out since `frames` was last cleared. */
  sent = () => this.all().length > 0
}

const tick = () => new Promise(r => setTimeout(r, 0))
const CREATE = 0x01, PLACE = 0x02, REMOVE = 0x04, LAYOUT = 0x10, SPATIAL = 0x20, LAYER = 0x22

test("zIndex travels in the spatial op, not layout", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const App = ({ z }: { z?: number }) =>
    createElement(View, { style: { width: 10, height: 10, zIndex: z } })
  root.renderSync(createElement(App, { z: 5 }))
  await tick()
  expect(t.ops().find(o => o.tag === SPATIAL)!.f).toEqual([4, 5])
  t.frames.length = 0
  root.renderSync(createElement(App, { z: -3 }))
  await tick()
  expect(t.ops()).toMatchObject([{ tag: SPATIAL, f: [4, -3] }])
  // Unset is 0, sent as a change like any other.
  t.frames.length = 0
  root.renderSync(createElement(App, {}))
  await tick()
  expect(t.ops()).toMatchObject([{ tag: SPATIAL, f: [4, 0] }])
  expect(t.ops().some(o => o.tag === LAYOUT)).toBe(false)
})

test("a z of 0 sends no spatial op at mount", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(View, { style: { width: 10, zIndex: 0 } }))
  await tick()
  expect(t.ops().some(o => o.tag === SPATIAL)).toBe(false)
})

test("a style of only zIndex has no layout: undefined, {zIndex}, undefined", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const App = ({ style }: { style?: { zIndex: number } }) => createElement(View, { style })
  root.renderSync(createElement(App, {}))
  await tick()
  for (const [style, z] of [[{ zIndex: 1 }, 1], [undefined, 0]] as const) {
    t.frames.length = 0
    root.renderSync(createElement(App, { style }))
    await tick()
    expect(t.all()).toMatchObject([{ tag: SPATIAL, f: [4, z] }])
  }
})

test("any number is a zIndex: rounded, clamped, NaN is 0", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const error = console.error
  console.error = () => {}
  try {
    for (const [zIndex, z] of [[1.5, 2], [1e12, 0x7fff_ffff], [-Infinity, -0x8000_0000], [NaN, 0]]) {
      t.frames.length = 0
      root.renderSync(createElement(View, { style: { zIndex } }))
      await tick()
      const spatial = t.all().find(o => o.tag === SPATIAL)
      expect(spatial?.f ?? [4, 0]).toEqual([4, z])
    }
  } finally {
    console.error = error
  }
})

/** The ops that open layer containers: [container, owner, z]. */
function layers(ops: Op[]) {
  return ops.filter(o => o.tag === LAYER).map(o => [
    o.id,
    o.f[0],
    ops.find(s => s.tag === SPATIAL && s.id === o.id)?.f[1] ?? 0,
  ])
}

test("a layer opens at the root level, owned by the layer it renders in", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(
    createElement(View, null,
      createElement(Layer, { z: 70 },
        createElement(View, { accessibilityLabel: "dialog" },
          createElement(Layer, { z: 50 }, createElement(View, { accessibilityLabel: "menu" }))))),
  )
  await tick()
  const ops = t.all()
  const [[dialog, dialogOwner, dialogZ], [menu, menuOwner, menuZ]] = layers(ops) as number[][]
  expect([dialogOwner, dialogZ]).toEqual([NIL, 70])
  expect([menuOwner, menuZ]).toEqual([dialog, 50])
  // Both containers sit at the end of the root level, in open order; the
  // app's root view goes in before them, though React placed it last.
  const rootPlaced = ops.filter(o => o.tag === PLACE && o.f[0] === NIL).map(o => [o.id, o.f[2]])
  expect(rootPlaced).toEqual([[dialog, NIL], [menu, NIL], [expect.any(Number), dialog]])
  // Each layer's content lives in its container.
  const parentOf = (id: number) => ops.find(o => o.tag === PLACE && o.id === id)!.f[0]
  const created = ops.filter(o => o.tag === CREATE).map(o => o.id)
  const inDialog = created.filter(id => parentOf(id) === dialog)
  const inMenu = created.filter(id => parentOf(id) === menu)
  expect(inDialog.length).toBe(1)
  expect(inMenu.length).toBe(1)
})

test("a layer's z updates in place; unmounting removes its container", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let setZ!: (z: number) => void
  let setOpen!: (open: boolean) => void
  function App() {
    const [z, sz] = useState(50)
    const [open, so] = useState(true)
    setZ = sz
    setOpen = so
    return open ? createElement(Layer, { z }, createElement(View)) : null
  }
  root.renderSync(createElement(App))
  await tick()
  const [[container]] = layers(t.all()) as number[][]
  t.frames.length = 0
  root.renderSync(createElement(App))
  setZ(90)
  await settle(t.sent)
  expect(t.all()).toMatchObject([{ tag: SPATIAL, id: container, f: [4, 90] }])
  t.frames.length = 0
  setOpen(false)
  await settle(t.sent)
  expect(t.all().some(o => o.tag === REMOVE && o.id === container)).toBe(true)
})

test("a layer closes with its last child and reopens on top", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let setShown!: (shown: boolean) => void
  function App() {
    const [shown, ss] = useState(true)
    setShown = ss
    return createElement(View, null,
      createElement(Layer, { z: 50 }, shown ? createElement(View) : null),
      createElement(Layer, { z: 50 }, createElement(View)))
  }
  root.renderSync(createElement(App))
  await tick()
  const [[first], [second]] = layers(t.all()) as number[][]
  t.frames.length = 0
  setShown(false)
  await settle(t.sent)
  expect(t.all().some(o => o.tag === REMOVE && o.id === first)).toBe(true)
  t.frames.length = 0
  setShown(true)
  await settle(t.sent)
  // Open order: it is now the newest layer, above the second.
  const ops = t.all()
  const [[again, owner]] = layers(ops) as number[][]
  expect(owner).toBe(NIL)
  expect(ops.find(o => o.tag === PLACE && o.id === again)!.f).toEqual([NIL, again, NIL])
  expect(again).not.toBe(second)
})

test("an empty layer sends nothing", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(View, null, createElement(Layer, { z: 60 })))
  await tick()
  expect(t.all().filter(o => o.tag === CREATE).length).toBe(1)
})

test("an owner stays open while a layer it owns is open", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let setShown!: (shown: boolean) => void
  function App() {
    const [shown, ss] = useState(true)
    setShown = ss
    return createElement(View, null,
      createElement(Layer, { z: 70 },
        shown ? createElement(View) : null,
        createElement(Layer, { z: 50 }, createElement(View))))
  }
  root.renderSync(createElement(App))
  await tick()
  const [[dialog], [menu, owner]] = layers(t.all()) as number[][]
  expect(owner).toBe(dialog)
  // The dialog's own child leaves; the menu it owns keeps it open.
  t.frames.length = 0
  setShown(false)
  await settle(t.sent)
  expect(t.all().some(o => o.tag === REMOVE && (o.id === dialog || o.id === menu))).toBe(false)
  t.frames.length = 0
  setShown(true)
  await settle(t.sent)
  const ops = t.all()
  expect(layers(ops)).toEqual([])
  expect(ops.find(o => o.tag === PLACE)!.f[0]).toBe(dialog)
})

test("closing cascades to an idle owner, and both reopen", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let setShown!: (shown: boolean) => void
  function App() {
    const [shown, ss] = useState(true)
    setShown = ss
    return createElement(Layer, { z: 70 },
      createElement(Layer, { z: 50 }, shown ? createElement(View) : null))
  }
  root.renderSync(createElement(App))
  await tick()
  const [[dialog, none], [menu, owner]] = layers(t.all()) as number[][]
  expect([none, owner]).toEqual([NIL, dialog])
  t.frames.length = 0
  setShown(false)
  await settle(t.sent)
  const removed = t.all().filter(o => o.tag === REMOVE).map(o => o.id)
  expect(removed).toEqual(expect.arrayContaining([dialog, menu]))
  t.frames.length = 0
  setShown(true)
  await settle(t.sent)
  const [[dialog2, none2], [, owner2]] = layers(t.all()) as number[][]
  expect([none2, owner2]).toEqual([NIL, dialog2])
})

test("an app root remounted while a layer is open lands below it", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let setKey!: (k: number) => void
  function App() {
    const [k, sk] = useState(0)
    setKey = sk
    return createElement(Fragment, null,
      createElement(View, { key: k }),
      createElement(Layer, { z: 50 }, createElement(View)))
  }
  root.renderSync(createElement(App))
  await tick()
  const [[container]] = layers(t.all()) as number[][]
  t.frames.length = 0
  setKey(1)
  await settle(t.sent)
  const placed = t.all().filter(o => o.tag === PLACE && o.f[0] === NIL)
  expect(placed.map(o => o.f[2])).toEqual([container])
})

test("a child inserted before another inside a layer", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let setFirst!: (first: boolean) => void
  function App() {
    const [first, sf] = useState(false)
    setFirst = sf
    return createElement(Layer, { z: 50 },
      first ? createElement(View, { key: "a" }) : null,
      createElement(View, { key: "b" }))
  }
  root.renderSync(createElement(App))
  await tick()
  const ops = t.all()
  const [[container]] = layers(ops) as number[][]
  const b = ops.find(o => o.tag === PLACE && o.f[0] === container)!.id
  t.frames.length = 0
  setFirst(true)
  await settle(t.sent)
  const a = t.all().find(o => o.tag === CREATE)!.id
  expect(t.all().find(o => o.tag === PLACE)!.f).toEqual([container, a, b])
})

test("Suspense hides and reveals a layer's children, keeping it open", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let wake!: () => void
  const pending = new Promise<void>(r => { wake = r })
  let suspend = false
  function Lazy() {
    if (suspend) throw pending
    return null
  }
  const app = () => createElement(View, null,
    createElement(Suspense, { fallback: null },
      createElement(Layer, { z: 50 }, createElement(View)),
      createElement(Lazy)))
  root.renderSync(app())
  await tick()
  const ops = t.all()
  const [[container]] = layers(ops) as number[][]
  const child = ops.find(o => o.tag === PLACE && o.f[0] === container)!.id
  // Hiding is display: none on the layer's child (a layout op).
  t.frames.length = 0
  suspend = true
  root.renderSync(app())
  await tick()
  expect(t.all().filter(o => o.tag !== LAYOUT)).toEqual([])
  expect(t.all().map(o => o.id)).toEqual([child])
  // Revealing restores its layout; the container never closed.
  t.frames.length = 0
  suspend = false
  wake()
  await pending
  root.renderSync(app())
  await tick()
  expect(t.all().filter(o => o.tag !== LAYOUT)).toEqual([])
  expect(t.all().map(o => o.id)).toEqual([child])
})
