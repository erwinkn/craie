import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, Pressable, Text, View, type PressableProps, type PressEvt, type PressOutEvt } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { ACTIVATE_SOURCE, EVENT_KIND, EVENT_MASK, MODS, PRESS_FLAG, PRESS_PHASE, ROLE } from "../src/wire.js"
import { readFrame } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_: (seq: number) => void) {}
  event?: (ev: UiEvent) => void
  onEvent(cb: (ev: UiEvent) => void) { this.event = cb }
  close() {}
  all() { return this.frames.flatMap(f => readFrame(f).ops) }
  /** The last INTERACTION op for `id`: [listener mask, flag byte]. */
  interaction(id: number) {
    const op = this.all().filter(o => o.tag === 0x60 && o.id === id).at(-1)
    return op && [op.f[0]!, op.f[1]!]
  }
}

const tick = () => new Promise(r => setTimeout(r, 0))
const created = (t: FakeTransport) => t.all().filter(o => o.tag === 0x01).map(o => o.id)
const FOCUSABLE = 1
const flag = (bits: number) => bits << 4

/** A PRESS or ACTIVATE record as native sends it (events.rs). */
function rec(kind: number, node: number, bits: number, o: { button?: number; mods?: number; span?: number; revision?: number } = {}): UiEvent {
  return {
    kind, node, generation: 0, revision: o.revision ?? 0, x: 350, y: 20, a: 50, b: 20,
    key: (o.mods ?? 0) | bits << 4 | (o.button ?? 0) << 8 | (o.span ?? 0) << 16, text: "",
  }
}
const activate = (node: number, source: number, o?: Parameters<typeof rec>[3]) =>
  rec(EVENT_KIND.activate, node, source, o)
const press = (node: number, phase: number, o?: Parameters<typeof rec>[3]) =>
  rec(EVENT_KIND.press, node, phase, { button: 1, ...o })

test("a Pressable sends its press flags, and listens only for what it handles", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const App = (p: PressableProps) => createElement(Pressable, p)
  root.renderSync(createElement(App, { onPress: () => {} }))
  await tick()
  const [id] = created(t)
  // Focusable and pressable; activate only (no raw pointer listener).
  expect(t.interaction(id!)).toEqual([EVENT_MASK.activate, FOCUSABLE | flag(PRESS_FLAG.pressable)])

  root.renderSync(createElement(App, { onPressIn: () => {}, onPressOut: () => {} }))
  await tick()
  expect(t.interaction(id!)![0]).toBe(EVENT_MASK.press)

  // preventFocusOnPress: keeps focus, and out of the Tab order unless
  // focusable says otherwise.
  root.renderSync(createElement(App, { preventFocusOnPress: true }))
  await tick()
  expect(t.interaction(id!)).toEqual([0, flag(PRESS_FLAG.pressable | PRESS_FLAG.keepFocus)])
  root.renderSync(createElement(App, { preventFocusOnPress: true, focusable: true }))
  await tick()
  expect(t.interaction(id!)![1]).toBe(FOCUSABLE | flag(PRESS_FLAG.pressable | PRESS_FLAG.keepFocus))

  // Disabled: swallows presses natively, and leaves the Tab order.
  root.renderSync(createElement(App, { disabled: true }))
  await tick()
  expect(t.interaction(id!)![1]).toBe(flag(PRESS_FLAG.pressable | PRESS_FLAG.disabled))

  // A plain View is no pressable.
  const v = new FakeTransport()
  createRoot(v).renderSync(createElement(View, { onPointerUp: () => {} }))
  await tick()
  expect(v.interaction(created(v)[0]!)![1]).toBe(0)
})

test("onPress comes from each activation source", async () => {
  const t = new FakeTransport()
  const got: PressEvt[] = []
  createRoot(t).renderSync(createElement(Pressable, { onPress: (e: PressEvt) => got.push(e) }))
  await tick()
  const [id] = created(t)
  t.event!(activate(id!, 0, { button: 1, mods: MODS.meta }))
  t.event!(activate(id!, 1))
  t.event!(activate(id!, 2))
  expect(got.map(e => [e.source, e.button, e.meta])).toEqual([
    ["pointer", 1, true],
    ["keyboard", 0, false],
    ["accessibility", 0, false],
  ])
  expect(ACTIVATE_SOURCE).toEqual(["pointer", "keyboard", "accessibility"])
  expect(got[0]).toMatchObject({ x: 350, y: 20, rx: 50, ry: 20 })
  // A raw pointer release is no press anymore.
  t.event!({ ...activate(id!, 0), kind: EVENT_KIND.pointerUp, key: 1 << 8 })
  expect(got.length).toBe(3)
})

test("onPressIn and onPressOut follow the press phases", async () => {
  const t = new FakeTransport()
  const log: string[] = []
  createRoot(t).renderSync(createElement(Pressable, {
    onPressIn: () => log.push("in"),
    onPressOut: (e: PressOutEvt) => log.push(e.cancelled ? "cancel" : "out"),
    onPress: () => log.push("press"),
  }))
  await tick()
  const [id] = created(t)
  t.event!(press(id!, PRESS_PHASE.in))
  t.event!(press(id!, PRESS_PHASE.out))
  t.event!(activate(id!, 0, { button: 1 }))
  t.event!(press(id!, PRESS_PHASE.in))
  t.event!(press(id!, PRESS_PHASE.cancel))
  expect(log).toEqual(["in", "out", "press", "in", "cancel"])
})

test("a nested Text with onPress is a pressable span", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const log: string[] = []
  const App = ({ link }: { link: boolean }) => createElement(Text, null,
    "Deploy failed: ",
    createElement(Text, link
      ? { onPress: (e: PressEvt) => log.push(`link ${e.source}`), onPressIn: () => log.push("link in") }
      : {},
    "see logs", createElement(Text, { fontWeight: "bold" }, " now")))
  root.renderSync(createElement(App, { link: true }))
  await tick()
  const [id] = created(t)
  // The root is no pressable itself; its spans are.
  const [mask, flags] = t.interaction(id!)!
  expect(mask).toBe(EVENT_MASK.activate | EVENT_MASK.press)
  expect(flags).toBe(0)
  const { spans } = readFrame(t.frames[0]!)
  // A span inside the link presses the link too.
  expect(spans.map(s => s.pressable)).toEqual([false, true, true])

  t.event!(press(id!, PRESS_PHASE.in, { span: 3, revision: 1 }))
  t.event!(activate(id!, 0, { button: 1, span: 2, revision: 1 }))
  expect(log).toEqual(["link in", "link pointer"])

  // Dropping onPress unmarks the span.
  root.renderSync(createElement(App, { link: false }))
  await tick()
  expect(t.interaction(id!)![0]).toBe(0)
  const f = t.frames.at(-1)!
  expect(readFrame(f).spans.map(s => s.pressable)).toEqual([false, false, false])
})

test("a root Text with onPress is a node-level pressable link, not focusable", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let n = 0
  root.renderSync(createElement(Text, { onPress: () => n++ }, "Open"))
  await tick()
  const [id] = created(t)
  expect(t.interaction(id!)).toEqual([EVENT_MASK.activate, flag(PRESS_FLAG.pressable)])
  t.event!(activate(id!, 2))
  expect(n).toBe(1)
  const role = () => t.all().filter(o => o.tag === 0x50 && o.id === id).at(-1)!.f[0]
  expect(role()).toBe(ROLE.link)
  // A role given wins; without onPress it is text again.
  root.renderSync(createElement(Text, { onPress: () => n++, accessibilityRole: "button" }, "Open"))
  await tick()
  expect(role()).toBe(ROLE.button)
  root.renderSync(createElement(Text, null, "Open"))
  await tick()
  expect(role()).toBe(ROLE.text)
})

test("a link in several spans joins them; two links side by side stay two", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(Text, null,
    "Deploy failed: ",
    createElement(Text, { onPress: () => {} }, "See ", createElement(Text, { fontWeight: 700 }, "logs")),
    createElement(Text, { onPress: () => {} }, "retry"),
  ))
  await tick()
  const { spans } = readFrame(t.frames[0]!)
  expect(spans.map(s => [s.pressable, s.pressJoins])).toEqual([
    [false, false], [true, false], [true, true], [true, false],
  ])
})

test("onPressIn or onPressOut alone makes no Text pressable", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(Text, { onPressIn: () => {} },
    "Open ", createElement(Text, { onPressOut: () => {} }, "now")))
  await tick()
  const [id] = created(t)
  expect(t.interaction(id!)).toEqual([EVENT_MASK.press, 0])
  expect(readFrame(t.frames[0]!).spans.map(s => s.pressable)).toEqual([false, false])
})

test("a press's out or cancel reaches the Text that heard it, across span tables", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const log: string[] = []
  const link = (name: string) => ({
    onPress: () => log.push(`${name} press`),
    onPressIn: () => log.push(`${name} in`),
    onPressOut: (e: PressOutEvt) => log.push(`${name} ${e.cancelled ? "cancel" : "out"}`),
  })
  const App = ({ before }: { before: boolean }) => createElement(Text, null,
    before ? createElement(Text, link("new"), "new ") : null,
    createElement(Text, link("logs"), "logs"))
  root.renderSync(createElement(App, { before: false }))
  await tick()
  const [id] = created(t)
  t.event!(press(id!, PRESS_PHASE.in, { span: 1, revision: 1 }))
  // A Text inserted before the link: span 1 is "new" now, and native
  // cancels the press with the table it began in.
  root.renderSync(createElement(App, { before: true }))
  await tick()
  t.event!(press(id!, PRESS_PHASE.cancel, { span: 1, revision: 1 }))
  expect(log).toEqual(["logs in", "logs cancel"])
})
