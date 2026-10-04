import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, Pressable, View, type BoxShadow } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
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

const PAINT = 0x30, VARIANTS = 0xb1
const tick = () => new Promise(r => setTimeout(r, 0))

// The kit's `button` elevation, as its native resolver emits it.
const button: BoxShadow[] = [
  { spreadDistance: 1, color: "#303030" },
  { offsetY: 4, blurRadius: 8, color: 0x0000_000a },
]

test("boxShadow sends the list in PAINT, then only when it changes", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const render = (shadow: BoxShadow[] | undefined, bg = "#202020") =>
    root.renderSync(createElement(View, { backgroundColor: bg, boxShadow: shadow }))
  render(button)
  await tick()
  const paint = t.take().find(o => o.tag === PAINT)!
  expect(paint.shadows).toEqual([
    { x: 0, y: 0, blur: 0, spread: 1, color: 0x3030_30ff, inset: false },
    { x: 0, y: 4, blur: 8, spread: 0, color: 0x0000_000a, inset: false },
  ])
  // A new array with the same shadows sends none; a fill change sends
  // the fill alone.
  render(button.map(s => ({ ...s })), "#303030")
  await tick()
  const again = t.take().filter(o => o.tag === PAINT)
  expect(again.length).toBe(1)
  expect(again[0]!.shadows).toBeUndefined()
  // Removing them sends an empty list.
  render(undefined, "#303030")
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.shadows).toEqual([])
})

test("what native would reject is clamped, and the list capped at 8", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const many: BoxShadow[] = Array.from({ length: 10 }, (_, i) => ({ offsetY: i, color: "#000" }))
  many[0] = { blurRadius: -3, offsetX: NaN, spreadDistance: 1e9, color: "#000", inset: true }
  root.renderSync(createElement(View, { boxShadow: many }))
  await tick()
  const s = t.take().find(o => o.tag === PAINT)!.shadows!
  expect(s.length).toBe(8)
  expect(s[0]).toEqual({ x: 0, y: 0, blur: 0, spread: 4096, color: 0x0000_00ff, inset: true })
})

test("a variant's boxShadow goes in its values (bit 15)", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(createElement(Pressable, {
    boxShadow: button,
    _hover: { boxShadow: [{ offsetY: 12, blurRadius: 24, color: 0x0000_0014 }] },
  }))
  await settle(() => t.frames.some(f => readFrame(f).ops.some(o => o.tag === VARIANTS)))
  const v = t.take().find(o => o.tag === VARIANTS)!.variants![0]!
  expect(v.values[0]! & 0x8000).toBe(0x8000)
  expect(v.shadows).toEqual([{ x: 0, y: 12, blur: 24, spread: 0, color: 0x0000_0014, inset: false }])
})

test("colors take the kit's rgba() form", async () => {
  const { color } = await import("../src/host.js")
  expect(color("rgba(0, 0, 0, 0.03)")).toBe(0x0000_0008)
  expect(color("rgba(255, 255, 255, 0.14)")).toBe(0xffff_ff24)
  expect(color("rgb(59, 130, 246)")).toBe(0x3b82_f6ff)
  expect(color("rgb(59 130 246 / 50%)")).toBe(0x3b82_f680)
  expect(color(" RGB(1, 2, 3) ")).toBe(0x0102_03ff)
  expect(color("Rgba( 0 , 0 , 0 , 1 )")).toBe(0x0000_00ff)
  expect(color(" #fff ")).toBe(0xffff_ffff)
  expect(() => color("rgba(1, 2)")).toThrow()
  expect(() => color("rgba(1, 2, x, 1)")).toThrow()
})
