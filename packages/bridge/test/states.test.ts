import { test, expect } from "bun:test"
import { createElement, StrictMode, Suspense, useState } from "react"
import { createRoot, defineStates, Layer, Path, Portal, Pressable, Text, TextInput, Vector, View } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { CURRENT, ENV_BIT, layoutKeys, STATE_BIT } from "../src/wire.js"
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
  expect(own[3]!.values[1]).toBe(1 << 11) // the height key alone
  // `_row` reads the named scope; a bare `_hover` the nearest (itself).
  const kid = tables.find(o => o.id === inner)!.variants!
  expect(kid.map(v => [v.terms.map(x => x.scope), v.values[0]])).toEqual([
    [[row!], 16],
    [[inner!], 4],
  ])
})

test("layout values travel per key: axes and sides on their own", async () => {
  const k = (...bits: number[]) => bits.reduce((m, b) => m | (1n << BigInt(b)), 0n)
  expect(layoutKeys({ height: 44 })).toBe(k(11))
  expect(layoutKeys({ padding: 4 })).toBe(k(16, 17, 18, 19))
  expect(layoutKeys({ padding: { left: 4, right: 4 } })).toBe(k(16, 17))
  expect(layoutKeys({ inset: 0, top: 3 })).toBe(k(28, 29, 30, 31))
  expect(layoutKeys({ top: 3 })).toBe(k(30))
  expect(layoutKeys({ gap: { height: 8 } })).toBe(k(9))
  expect(layoutKeys({ overflow: { y: "scroll" } })).toBe(k(37))
  expect(layoutKeys({ flexGrow: 1 })).toBe(k(33))

  // px 16, py 12, narrow px 4, compact py 6: each variant sends only
  // its sides, so at compact width both apply (native composes them).
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(View, {
    style: { padding: { left: 16, right: 16, top: 12, bottom: 12 } },
    _narrow: { style: { padding: { left: 4, right: 4 } } },
    _compact: { style: { padding: { top: 6, bottom: 6 } } },
  }))
  await tick()
  const [table] = t.ops().filter(o => o.tag === 0xb1)
  expect(table!.variants!.map(v => [v.env, v.values[1]])).toEqual([
    [ENV_BIT.narrow, Number(k(16, 17))],
    [ENV_BIT.compact, Number(k(18, 19))],
  ])
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

test("a Portal or Layer starts a new scope chain", async () => {
  const errors: string[] = []
  const log = console.error
  console.error = (m: string) => errors.push(m)
  const t = new FakeTransport()
  try {
    createRoot(t).renderSync(createElement(Pressable, { group: "menu", expanded: true },
      createElement(Portal, {},
        // No scope inside: the opener's is not in reach.
        createElement(View, { _menu: { _expanded: { style: { opacity: 1 } } }, _pressed: { borderRadius: 2 } })),
      createElement(Layer, { z: 50 },
        // Nor in a Layer: this reads nothing.
        createElement(View, { _expanded: { style: { opacity: 1 } } }),
        createElement(Pressable, {},
          createElement(Text, { _hover: { color: "#fff" } }, "item")))))
    await tick()
  } finally {
    console.error = log
  }
  expect(errors.filter(e => e.includes('no group "menu" above')).length).toBe(1)
  expect(errors.some(e => e.includes("_pressed needs a scope"))).toBe(true)
  expect(errors.some(e => e.includes("_expanded needs a scope"))).toBe(true)
  const ops = t.ops()
  // Two scopes: the expanded opener and the item inside the layer.
  const states = ops.filter(o => o.tag === 0xb0)
  expect(states.map(o => o.bits).sort()).toEqual([0n, bit("expanded")])
  const item = states.find(o => o.bits === 0n)!.id
  // Only the item's Text has a table, and it reads the item.
  const tables = ops.filter(o => o.tag === 0xb1)
  expect(tables.map(o => o.id)).toEqual(created(ops, 1))
  expect(tables[0]!.variants!.map(v => v.terms.map(x => [x.scope, x.mask]))).toEqual([[[item, bit("hover")]]])
})

test("a TextInput is its own scope, with no disabled", async () => {
  const errors: string[] = []
  const log = console.error
  console.error = (m: string) => errors.push(m)
  const t = new FakeTransport()
  // A kit's props may carry it past the types.
  const kit = { disabled: true }
  try {
    createRoot(t).renderSync(createElement(Pressable, {},
      createElement(TextInput, {
        ...kit,
        states: { unread: true },
        _focusVisible: { borderColor: "#4c8dff" },
        _unread: { borderWidth: 2 },
      })))
    await tick()
  } finally {
    console.error = log
  }
  expect(errors.some(e => e.includes("TextInput takes no disabled"))).toBe(true)
  const ops = t.ops()
  const input = created(ops, 2)[0]!
  // The input sends its own app bits, not disabled, and its variants
  // read only it.
  expect(ops.find(o => o.tag === 0xb0 && o.id === input)!.bits! & bit("disabled")).toBe(0n)
  const table = ops.find(o => o.tag === 0xb1 && o.id === input)!
  expect(table.variants!.map(v => v.terms.map(x => [x.scope, x.mask]))).toEqual([
    [[input, bit("focusVisible")]],
    [[input, 1n << 0n]],
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

test("toggling group keeps the children mounted", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const app = (group?: string) => createElement(View, { group },
    createElement(View, { _g: { _hover: { borderRadius: 4 } } }), createElement(Text, {}, "a"))
  const log = console.error
  console.error = () => {} // `_g` has no scope yet
  try {
    root.renderSync(app())
    await tick()
  } finally {
    console.error = log
  }
  root.renderSync(app("g"))
  await tick()
  const ops = t.ops()
  expect(ops.filter(o => o.tag === 0x01 || o.tag === 0x04)).toEqual([])
  // The View is now a scope, and the child's `_g` finds it.
  const scope = ops.find(o => o.tag === 0xb0)!.id
  const table = ops.find(o => o.tag === 0xb1)!
  expect(table.variants!.map(v => v.terms.map(x => x.scope))).toEqual([[scope]])
})

test("StrictMode mounts one scope and its tables point at it", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(StrictMode, null,
    createElement(Pressable, { selected: true }, createElement(View, { _selected: { borderRadius: 4 } }))))
  await tick()
  const ops = t.all()
  const states = ops.filter(o => o.tag === 0xb0)
  expect(states.map(o => o.bits)).toEqual([bit("selected")])
  expect(created(ops, 0)).toContain(states[0]!.id)
  const table = ops.find(o => o.tag === 0xb1)!
  expect(table.variants![0]!.terms.map(x => x.scope)).toEqual([states[0]!.id])
})

test("Suspense: a hidden node's variants set no display, and come back on reveal", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let wake!: () => void
  const pending = new Promise<void>(r => { wake = r })
  let suspend = false
  function Lazy() {
    if (suspend) throw pending
    return null
  }
  const app = () => createElement(Pressable, {},
    createElement(Suspense, { fallback: null },
      createElement(View, { _hover: { style: { display: "flex", width: 10 } } }),
      createElement(Lazy)))
  root.renderSync(app())
  await tick()
  const key = (ops: Op[]) => ops.filter(o => o.tag === 0xb1).map(o => o.variants!.map(v => v.values[1]))
  const display = 1 << 0, width = 1 << 10
  expect(key(t.all())).toEqual([[display | width]])
  t.frames.length = 0
  suspend = true
  root.renderSync(app())
  await tick()
  expect(key(t.all())).toEqual([[width]])
  t.frames.length = 0
  suspend = false
  wake()
  await pending
  root.renderSync(app())
  await tick()
  expect(key(t.all())).toEqual([[display | width]])
})

test("variant keys that do not apply are logged once each", async () => {
  const errors: string[] = []
  const log = console.error
  console.error = (m: string) => errors.push(m)
  try {
    // Types reject these; untyped callers get the log.
    const t = new FakeTransport()
    createRoot(t).renderSync(createElement(Pressable, {},
      createElement(View, { _hover: { pointerEvents: "none", visibility: "hidden", style: { zIndex: 2 } } } as any),
      createElement(View, { _hover: { pointerEvents: "none" } } as any)))
    await tick()
  } finally {
    console.error = log
  }
  expect(errors.filter(e => e.includes(`"pointerEvents"`)).length).toBe(1)
  expect(errors.filter(e => e.includes(`"visibility"`)).length).toBe(1)
  expect(errors.some(e => e.includes("style.zIndex"))).toBe(true)
})

test("a disabled Pressable is not focusable; border color and width travel apart", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const app = (disabled: boolean) => createElement(Pressable, {
    disabled,
    _hover: { borderColor: "#fff" },
    _focusVisible: { borderWidth: 2 },
  })
  root.renderSync(app(true))
  await tick()
  const flags = (ops: Op[]) => ops.filter(o => o.tag === 0x60).map(o => o.f[1]! & 1)
  expect(flags(t.ops())).toEqual([0])
  const table = t.ops().find(o => o.tag === 0xb1)!
  expect(table.variants!.map(v => v.values)).toEqual([[2, 0xffff_ffff], [128, 2]])
  root.renderSync(app(false))
  await tick()
  expect(flags(t.ops())).toEqual([1])
})

test("a Text with no color inherits, white when nothing above sets one", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(Text, {}, "a"))
  await tick()
  const f = readFrame(t.frames[0]!)
  expect(f.spans.map(s => [s.inheritColor, s.color])).toEqual([[true, 0xffff_ffff]])
  expect(f.ops.filter(o => o.tag === 0xb3)).toEqual([])
})

test("a Portal's content starts fresh: no COLOR, no scopes", async () => {
  const log = console.error
  console.error = () => {} // `_menu` is out of reach
  const t = new FakeTransport()
  try {
    createRoot(t).renderSync(createElement(Pressable, { color: "#9aa0aa", group: "menu" },
      createElement(Portal, {}, createElement(Text, { _menu: { _hover: { color: "#fff" } } }, "item"))))
    await tick()
  } finally {
    console.error = log
  }
  const ops = t.ops()
  const menu = ops.find(o => o.tag === 0xb0)!.id
  // The one COLOR is on the Pressable; the Text is a window root, so
  // natively it has no COLOR above and draws its own (white) color.
  expect(ops.filter(o => o.tag === 0xb3).map(o => o.id)).toEqual([menu])
  const text = created(ops, 1)[0]!
  expect(ops.find(o => o.tag === 0x02 && o.id === text)!.f[0]).toBe(0xffff_ffff)
  expect(ops.filter(o => o.tag === 0xb1)).toEqual([])
})

// DF-24: an icon's currentColor is the node's inherited color, resolved
// natively, so the hover recolors it as it does the label, with no JS.
test("currentColor inherits: a hover recolors icon and label alike", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(Pressable, { color: "#9aa0aa", _hover: { color: "#fff" } },
    createElement(Vector, { viewBox: "0 0 24 24" }, createElement(Path, { d: "M4 12h16", stroke: "currentColor" })),
    createElement(Text, {}, "Label")))
  await tick()
  const ops = t.ops()
  const [pressable] = created(ops, 0)
  expect(ops.filter(o => o.tag === 0xb3).map(o => [o.id, ...o.f])).toEqual([[pressable, 1, 0x9aa0_aaff]])
  expect(ops.find(o => o.tag === 0xb1 && o.id === pressable)!.variants![0]!.values).toEqual([8, 1, 0xffff_ffff])
  const shape = ops.find(o => o.tag === 0x72)!.shapes![0]!
  // kind, rule, join, cap, current, fill, stroke, ...: the stroke is
  // flagged, its color a white tint.
  expect([shape.f[4], shape.f[6]]).toEqual([CURRENT.stroke, 0xffff_ffff])
  // A Vector's own color is its COLOR, variants included.
  const u = new FakeTransport()
  createRoot(u).renderSync(createElement(Pressable, {},
    createElement(Vector, { viewBox: "0 0 24 24", color: "#f00", _hover: { color: "#0f0" } },
      createElement(Path, { d: "M4 12h16", stroke: "currentColor" }))))
  await tick()
  const vops = u.ops()
  const [vector] = created(vops, 5)
  expect(vops.filter(o => o.tag === 0xb3).map(o => [o.id, ...o.f])).toEqual([[vector, 1, 0xff00_00ff]])
  expect(vops.find(o => o.tag === 0xb1 && o.id === vector)!.variants![0]!.values).toEqual([8, 1, 0x00ff_00ff])
})

// DF-14: an input's text color is its COLOR, own or inherited, so its
// variants apply; nothing is left in the input config.
test("a TextInput's color is its COLOR, variants included", async () => {
  const errors: unknown[] = []
  const log = console.error
  console.error = (...a: unknown[]) => { errors.push(a.join(" ")) }
  try {
    const t = new FakeTransport()
    createRoot(t).renderSync(createElement(Pressable, { color: "#9aa0aa" },
      createElement(TextInput, { color: "#ccc", _hover: { color: "#fff" } }),
      createElement(TextInput, {})))
    await tick()
    const ops = t.ops()
    const [pressable] = created(ops, 0)
    const [own, inherits] = created(ops, 2)
    expect(ops.filter(o => o.tag === 0xb3).map(o => [o.id, ...o.f]))
      .toEqual([[pressable, 1, 0x9aa0_aaff], [own, 1, 0xcccc_ccff]])
    // The input is a scope, so its `_hover` reads its own hover.
    const variant = ops.find(o => o.tag === 0xb1 && o.id === own)!.variants![0]!
    expect(variant.terms.map(x => [x.scope, x.mask])).toEqual([[own, bit("hover")]])
    expect(variant.values).toEqual([8, 1, 0xffff_ffff])
    expect(ops.some(o => o.tag === 0xb1 && o.id === inherits)).toBe(false)
    // font size, flags: no color.
    expect(ops.find(o => o.tag === 0x41 && o.id === own)!.f).toEqual([14, 4])
    expect(errors).toEqual([])
  } finally {
    console.error = log
  }
})
