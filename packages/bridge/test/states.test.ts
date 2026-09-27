import { test, expect } from "bun:test"
import { createElement, useState } from "react"
import { createRoot, defineStates, Portal, Pressable, Text, View } from "../src/index.js"
import { pairedLayout } from "../src/host.js"
import type { Transport, UiEvent } from "../src/host.js"
import { ENV_BIT, STATE_BIT } from "../src/wire.js"
import { readFrame, type Op } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_: (seq: number) => void) {}
  event?: (ev: UiEvent) => void
  onEvent(cb: (ev: UiEvent) => void) { this.event = cb }
  close() {}
  ops(i = -1) { return readFrame(this.frames.at(i)!).ops }
  all() { return this.frames.flatMap(f => readFrame(f).ops) }
}

const tick = () => new Promise(r => setTimeout(r, 0))
const bit = (name: keyof typeof STATE_BIT) => 1n << BigInt(STATE_BIT[name])
const created = (ops: Op[], kind: number) => ops.filter(o => o.tag === 0x01 && o.f[0] === kind).map(o => o.id)

defineStates(["unread", "streaming"])

test("_ keys flatten depth first; nesting ANDs; env and scope names apply", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(
    createElement(Pressable, {
      group: "row",
      selected: true,
      states: { unread: true },
      backgroundColor: "#1b1d22",
      style: { width: 200, height: 36 },
      _hover: { backgroundColor: "#24272e" },
      _selected: { backgroundColor: "#2d3240", _hover: { backgroundColor: "#343a4a" } },
      _narrow: { style: { height: 44 } },
    },
      createElement(Pressable, {
        _row: { _hover: { style: { opacity: 1 } } },
        _hover: { borderRadius: 4 },
      })),
  )
  await tick()
  const ops = t.ops()
  const [row, inner] = created(ops, 0)
  // The scopes: the row with its app bits (selected, custom bit 0), the
  // inner Pressable with none.
  const states = ops.filter(o => o.tag === 0xb0)
  expect(states.map(o => [o.id, o.bits])).toEqual([[row!, bit("selected") | 1n], [inner!, 0n]])
  const tables = ops.filter(o => o.tag === 0xb1)
  const own = tables.find(o => o.id === row)!.variants!
  expect(own.map(v => [v.terms.map(x => [x.scope, x.mask]), v.env, v.values[0]])).toEqual([
    [[[row!, bit("hover")]], 0, 1],
    [[[row!, bit("selected")]], 0, 1],
    [[[row!, bit("selected") | bit("hover")]], 0, 1],
    [[], ENV_BIT.narrow, 64],
  ])
  expect(own[3]!.values[1]).toBe(1 << 9) // SIZE: height travels with width
  // `_row` reads the named scope; a bare `_hover` the nearest (itself).
  const kid = tables.find(o => o.id === inner)!.variants!
  expect(kid.map(v => [v.terms.map(x => x.scope), v.values[0]])).toEqual([
    [[row!], 16],
    [[inner!], 4],
  ])
})

test("pairing fills the rest of each wire field from the base", () => {
  expect(pairedLayout({ height: 44 }, { width: 200, height: 36 })).toEqual({ width: 200, height: 44 })
  expect(pairedLayout({ padding: { left: 4 } }, { padding: 12 }))
    .toEqual({ padding: { left: 4, right: 12, top: 12, bottom: 12 } })
  expect(pairedLayout({ top: 3 }, { inset: 1, left: 2 }))
    .toEqual({ left: 2, right: 1, top: 3, bottom: 1 })
  expect(pairedLayout({ gap: { height: 8 } }, { gap: 4 })).toEqual({ gap: { width: 4, height: 8 } })
  expect(pairedLayout({ flexGrow: 1 }, { width: 5 })).toEqual({ flexGrow: 1 })
})

test("the seal sends a table only when its signature changes", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const App = ({ hover, other }: { hover?: string; other: number }) =>
    createElement(Pressable, {
      accessibilityLabel: String(other),
      ...(hover ? { _hover: { backgroundColor: hover } } : {}),
    })
  root.renderSync(createElement(App, { hover: "#fff", other: 0 }))
  await tick()
  expect(t.ops().filter(o => o.tag === 0xb1).length).toBe(1)
  // Unrelated props change: no table.
  root.renderSync(createElement(App, { hover: "#fff", other: 1 }))
  await tick()
  expect(t.ops().map(o => o.tag)).toEqual([0x51])
  root.renderSync(createElement(App, { hover: "#000", other: 1 }))
  await tick()
  expect(t.ops().map(o => o.tag)).toEqual([0xb1])
  // No `_` keys left: an empty table restores the base.
  root.renderSync(createElement(App, { other: 1 }))
  await tick()
  const [op] = t.ops()
  expect([op!.tag, op!.variants!.length]).toEqual([0xb1, 0])
  root.renderSync(createElement(App, { other: 2 }))
  await tick()
  expect(t.ops().map(o => o.tag)).toEqual([0x51])
})

test("state changes send STATES alone", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const App = ({ on }: { on: boolean }) =>
    createElement(View, {
      group: true, selected: on, states: { streaming: on },
      _selected: { backgroundColor: "#2d3240" },
    })
  root.renderSync(createElement(App, { on: false }))
  await tick()
  root.renderSync(createElement(App, { on: true }))
  await tick()
  const [op] = t.ops()
  expect(t.ops().length).toBe(1)
  expect([op!.tag, op!.bits]).toEqual([0xb0, bit("selected") | 2n])
})

test("unknown keys and scope-less states are logged once and skipped", async () => {
  const errors: string[] = []
  const log = console.error
  console.error = (m: string) => errors.push(m)
  try {
    const t = new FakeTransport()
    createRoot(t).renderSync(createElement(View, {},
      createElement(View, { _bogus: { backgroundColor: "#fff" } }),
      createElement(View, { _bogus: { backgroundColor: "#000" } }),
      createElement(View, { _hover: { backgroundColor: "#fff" }, _narrow: { backgroundColor: "#111" } })))
    await tick()
    const tables = t.ops().filter(o => o.tag === 0xb1)
    // Only the narrow variant survives.
    expect(tables.map(o => o.variants!.map(v => v.env))).toEqual([[ENV_BIT.narrow]])
  } finally {
    console.error = log
  }
  expect(errors.filter(e => e.includes("_bogus")).length).toBe(1)
  expect(errors.some(e => e.includes("_hover needs a scope"))).toBe(true)
})

test("text inherits color: COLOR on the element, INHERIT_COLOR on its spans", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const App = ({ c }: { c: string }) =>
    createElement(View, { color: c },
      createElement(Text, { _unread: { color: "#fff" } }, "a",
        createElement(Text, { color: "#f00" }, "b"),
        createElement(Text, { fontSize: 20 }, "c")))
  root.renderSync(createElement(Pressable, { states: { unread: true } }, createElement(App, { c: "#9aa0aa" })))
  await tick()
  const f = readFrame(t.frames[0]!)
  expect(f.spans.map(s => [s.inheritColor, s.color])).toEqual([
    [true, 0xffff_ffff], [false, 0xff00_00ff], [true, 0xffff_ffff],
  ])
  const colors = f.ops.filter(o => o.tag === 0xb3)
  expect(colors.map(o => o.f)).toEqual([[1, 0x9aa0_aaff]])
  const text = f.ops.find(o => o.tag === 0xb1)!
  expect(text.variants![0]!.values).toEqual([8, 1, 0xffff_ffff])
  // A new color is one COLOR op: the paragraph stays.
  root.renderSync(createElement(Pressable, { states: { unread: true } }, createElement(App, { c: "#000" })))
  await tick()
  expect(t.ops().map(o => [o.tag, ...o.f])).toEqual([[0xb3, 1, 0xff]])
})

test("a Portal's content keeps its owner's scope", async () => {
  const t = new FakeTransport()
  function Menu() {
    const [open] = useState(true)
    return createElement(Pressable, { group: "menu" },
      open && createElement(Portal, {},
        createElement(View, { _menu: { _expanded: { style: { opacity: 1 } } }, _hover: { borderRadius: 2 } })))
  }
  createRoot(t).renderSync(createElement(Menu))
  await tick()
  const ops = t.ops()
  // The portal's content commits first; the menu is the scope.
  const menu = ops.find(o => o.tag === 0xb0)!.id
  const overlay = created(ops, 0).find(id => id !== menu)
  // The overlay is a window root, not the menu's child.
  expect(ops.find(o => o.tag === 0x02 && o.id === overlay)!.f[0]).toBe(0xffff_ffff)
  const table = ops.find(o => o.tag === 0xb1 && o.id === overlay)!
  expect(table.variants!.map(v => v.terms.map(x => [x.scope, x.mask]))).toEqual([
    [[menu!, bit("expanded")]],
    [[menu!, bit("hover")]],
  ])
})

test("disabled stops onPress; color transitions travel as prop 8", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let presses = 0
  const App = ({ disabled }: { disabled: boolean }) => createElement(Pressable, {
    disabled,
    onPress: () => presses++,
    style: { transition: { color: { duration: 100 } } },
  })
  root.renderSync(createElement(App, { disabled: true }))
  await tick()
  const ops = t.ops()
  expect(ops.find(o => o.tag === 0xb0)!.bits).toBe(bit("disabled"))
  expect(ops.find(o => o.tag === 0xa0)!.f.slice(0, 2)).toEqual([1, 8])
  const up = { kind: 3, node: 0, generation: 0, revision: 0, x: 0, y: 0, a: 0, b: 0, key: 1 << 8, text: "" }
  t.event!(up)
  expect(presses).toBe(0)
  root.renderSync(createElement(App, { disabled: false }))
  await tick()
  t.event!(up)
  expect(presses).toBe(1)
})
