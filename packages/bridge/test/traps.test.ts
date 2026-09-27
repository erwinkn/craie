import { test, expect } from "bun:test"
import { createElement, useState } from "react"
import { createRoot, FocusTrap, Layer, TextInput, View } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { INTERACTION, NIL, TRAP } from "../src/wire.js"
import { readFrame, type Op } from "./crw2.js"
import { settle } from "./settle.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_cb: (seq: number) => void) {}
  onEvent(_cb: (ev: UiEvent) => void) {}
  close() {}
  all() { return this.frames.flatMap(f => readFrame(f).ops) }
  sent = () => this.all().length > 0
}

const tick = () => new Promise(r => setTimeout(r, 0))
const CREATE = 0x01, PLACE = 0x02, LAYER = 0x22, LABEL = 0x51, INTERACT = 0x60, TRAP_OP = 0x62
const ALL = TRAP.active | TRAP.modal | TRAP.autoFocus | TRAP.restoreFocus

/** The node labelled `label`. */
const labelled = (ops: Op[], label: string) => ops.find(o => o.tag === LABEL && o.s === label)!.id
/** The last interaction flags sent for `id`. */
const flagsOf = (ops: Op[], id: number) => ops.filter(o => o.tag === INTERACT && o.id === id).at(-1)?.f[1]

test("inert and autoFocus travel as interaction flag bits", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(
    createElement(View, null,
      createElement(View, { inert: true, accessibilityLabel: "gone" }),
      createElement(View, { autoFocus: true, focusable: true, accessibilityLabel: "first" }),
      createElement(TextInput, { autoFocus: true, accessibilityLabel: "field" })),
  )
  await tick()
  const ops = t.all()
  expect(flagsOf(ops, labelled(ops, "gone"))).toBe(INTERACTION.inert)
  expect(flagsOf(ops, labelled(ops, "first"))).toBe(INTERACTION.focusable | INTERACTION.autoFocus)
  expect(flagsOf(ops, labelled(ops, "field"))! & INTERACTION.autoFocus).toBe(INTERACTION.autoFocus)
})

test("a FocusTrap sends its flags, and again when they change", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let set!: (p: { active?: boolean; modal?: boolean }) => void
  function App() {
    const [p, s] = useState<{ active?: boolean; modal?: boolean }>({})
    set = s
    return createElement(FocusTrap, p, createElement(View, { focusable: true }))
  }
  root.renderSync(createElement(App))
  await tick()
  const [op] = t.all().filter(o => o.tag === TRAP_OP)
  expect(op!.f).toEqual([TRAP.active | TRAP.autoFocus | TRAP.restoreFocus])
  t.frames.length = 0
  set({ modal: true })
  await settle(t.sent)
  expect(t.all().filter(o => o.tag === TRAP_OP)).toMatchObject([{ id: op!.id, f: [ALL] }])
  t.frames.length = 0
  set({ active: false, modal: true })
  await settle(t.sent)
  expect(t.all().filter(o => o.tag === TRAP_OP)).toMatchObject([{ id: op!.id, f: [ALL & ~TRAP.active] }])
})

// DF-19: a layer opened inside a FocusTrap is owned by the trap's node;
// outside one, by the enclosing layer's container; at the top, by none.
test("a layer's owner is the nearest FocusTrap, else the enclosing layer", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(
    createElement(View, null,
      createElement(Layer, { z: 70 },
        createElement(FocusTrap, { modal: true },
          createElement(View, { accessibilityLabel: "more" },
            createElement(Layer, { z: 50 }, createElement(View, { accessibilityLabel: "menu" })))),
        createElement(Layer, { z: 80 }, createElement(View, { accessibilityLabel: "toast" })))),
  )
  await tick()
  const ops = t.all()
  const trap = ops.find(o => o.tag === TRAP_OP)!.id
  const parentOf = (id: number) => ops.find(o => o.tag === PLACE && o.id === id)!.f[0]!
  const ownerOf = (label: string) => {
    const container = parentOf(labelled(ops, label))
    return ops.find(o => o.tag === LAYER && o.id === container)!.f[0]
  }
  const dialog = parentOf(trap)
  expect(ops.find(o => o.tag === LAYER && o.id === dialog)!.f[0]).toBe(NIL)
  expect(ownerOf("menu")).toBe(trap)
  expect(ownerOf("toast")).toBe(dialog)
  // Every layer op follows the creation of its owner.
  const at = (tag: number, id: number) => ops.findIndex(o => o.tag === tag && o.id === id)
  expect(at(LAYER, parentOf(labelled(ops, "menu")))).toBeGreaterThan(at(CREATE, trap))
})

// A menu opened in a later commit than its FocusTrap still finds it
// through context.
test("a layer opened after its FocusTrap is owned by it", async () => {
  const t = new FakeTransport()
  let open!: () => void
  function Dialog() {
    const [menu, setMenu] = useState(false)
    open = () => setMenu(true)
    return createElement(FocusTrap, { modal: true },
      createElement(View, { focusable: true },
        menu && createElement(Layer, { z: 50 }, createElement(View, { accessibilityLabel: "menu" }))))
  }
  createRoot(t).renderSync(createElement(Layer, { z: 70 }, createElement(Dialog)))
  await tick()
  const trap = t.all().find(o => o.tag === TRAP_OP)!.id
  t.frames.length = 0
  open()
  await settle(t.sent)
  const ops = t.all()
  const container = ops.find(o => o.tag === PLACE && o.id === labelled(ops, "menu"))!.f[0]
  expect(ops.find(o => o.tag === LAYER && o.id === container)!.f[0]).toBe(trap)
})
