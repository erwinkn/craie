import { test, expect } from "bun:test"
import { createElement, useEffect } from "react"
import { createRoot, Text, useWindow, View, type HostNode, type WindowState } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { EVENT_KIND, EVENT_MASK, NIL, WINDOW_BIT } from "../src/wire.js"
import { readFrame, type Op } from "./crw2.js"
import { settle } from "./settle.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_: (seq: number) => void) {}
  event?: (ev: UiEvent) => void
  onEvent(cb: (ev: UiEvent) => void) { this.event = cb }
  close() {}
  take(): Op[] {
    const ops = this.frames.flatMap(f => readFrame(f).ops)
    this.frames.length = 0
    return ops
  }
}

const INTERACTION = 0x60, COMMAND = 0x80, MEASURE = 6, PRESENT = 7
const tick = () => new Promise(r => setTimeout(r, 0))
const ev = (kind: number, node: number, fields: Partial<UiEvent> = {}): UiEvent =>
  ({ kind, node, generation: 0, revision: 0, x: 0, y: 0, a: 0, b: 0, key: 0, text: "", ...fields })

test("onLayout sets the layout listener bit and hears the node's box", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const seen: unknown[] = []
  root.renderSync(createElement(View, null,
    createElement(Text, { onLayout: (e: any) => seen.push([e.target.type, e.x, e.y, e.width, e.height]) }, "hi")))
  await tick()
  const listen = t.take().find(o => o.tag === INTERACTION && (o.f[0]! & EVENT_MASK.layout))
  expect(listen).toBeDefined()
  t.event!(ev(EVENT_KIND.layout, listen!.id, { x: 4, y: 8, a: 120, b: 18 }))
  expect(seen).toEqual([["text", 4, 8, 120, 18]])
})

test("measure resolves by request, with null when native could not measure", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let node: HostNode | null = null
  root.renderSync(createElement(View, { ref: (n: HostNode | null) => { node = n } }))
  await tick()
  t.take()
  const first = node!.measure()
  const second = node!.measure()
  await tick()
  const cmds = t.take().filter(o => o.tag === COMMAND && o.f[0] === MEASURE)
  expect(cmds.map(o => o.id)).toEqual([node!.id, node!.id])
  const [a, b] = cmds.map(o => o.f[1]!)
  expect(a).not.toBe(b)
  // Answers go by request, whatever the node's generation is now.
  t.event!(ev(EVENT_KIND.measure, node!.id, { key: b, generation: 9, revision: 0 }))
  t.event!(ev(EVENT_KIND.measure, node!.id, { key: a, revision: 1, x: 1, y: 2, a: 30, b: 40 }))
  expect(await first).toEqual({ x: 1, y: 2, width: 30, height: 40 })
  expect(await second).toBeNull()
})

test("presented and capture send a window PRESENT and resolve with the frame", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(createElement(View))
  await tick()
  t.take()
  const shown = root.host.presented()
  const shot = root.host.capture("/tmp/a.png", { rest: true })
  await tick()
  const cmds = t.take().filter(o => o.tag === COMMAND && o.f[0] === PRESENT)
  expect(cmds.map(o => [o.id, o.f[2], o.s])).toEqual([[NIL, 0, undefined], [NIL, 1, "/tmp/a.png"]])
  t.event!(ev(EVENT_KIND.presented, NIL, { key: cmds[0]!.f[1]!, revision: 7, x: 800, y: 600 }))
  t.event!(ev(EVENT_KIND.presented, NIL, { key: cmds[1]!.f[1]!, revision: 9, x: 800, y: 600, text: "disk full" }))
  expect(await shown).toEqual({ frame: 7, width: 800, height: 600 })
  await expect(shot).rejects.toThrow("capture failed: disk full")
})

test("useWindow follows the window events", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const seen: (WindowState | null)[] = []
  function App() {
    const w = useWindow()
    useEffect(() => { seen.push(w) })
    return createElement(View)
  }
  root.renderSync(createElement(App))
  await tick()
  expect(seen).toEqual([null])
  t.event!(ev(EVENT_KIND.window, NIL, { x: 800, y: 600, a: 2, key: WINDOW_BIT.focused | WINDOW_BIT.visible }))
  await settle(() => seen.length > 1)
  expect(seen.at(-1)).toEqual({ width: 800, height: 600, scale: 2, focused: true, visible: true, dark: false })
  t.event!(ev(EVENT_KIND.window, NIL, { x: 800, y: 600, a: 2, key: WINDOW_BIT.dark }))
  await settle(() => seen.length > 2)
  expect(seen.at(-1)).toEqual({ width: 800, height: 600, scale: 2, focused: false, visible: false, dark: true })
  expect(root.host.window()?.dark).toBe(true)
})
