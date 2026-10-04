import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, View, type ViewProps } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { readFrame, type Op } from "./crw2.js"

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

const PAINT = 0x30
const tick = () => new Promise(r => setTimeout(r, 0))

test("a side's width or color sends all four sides; unset ones fall back to the uniform border natively", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const render = (props: ViewProps) => root.renderSync(createElement(View, props))
  // The kit's divider: a bottom line, in the uniform color.
  render({ borderBottomWidth: 1, borderColor: "#202020" })
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides).toEqual({ widths: [0, 0, 1, 0], colors: [0, 0, 0, 0], fallback: 0b1111_1011 })
  // One side's color over a uniform border.
  render({ borderWidth: 2, borderColor: "#202020", borderLeftColor: "#ff0000" })
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides).toEqual({
    widths: [0, 0, 0, 0],
    colors: [0, 0, 0, 0xff00_00ff],
    fallback: 0b0111_1111,
  })
  // The same sides again send none, and so does a uniform color change
  // (native resolves fallen-back sides from it); dropping them sends
  // every side fallen back: none.
  render({ borderWidth: 2, borderColor: "#202020", borderLeftColor: "#ff0000" })
  await tick()
  expect(t.take().filter(o => o.tag === PAINT).length).toBe(0)
  render({ borderWidth: 2, borderColor: "#00ff00", borderLeftColor: "#ff0000" })
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides).toBeUndefined()
  render({ borderWidth: 2, borderColor: "#202020" })
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides).toEqual({ widths: [0, 0, 0, 0], colors: [0, 0, 0, 0], fallback: 0xff })
  // Explicit zeros are sides, not none.
  render({ borderWidth: 2, borderColor: "#202020", borderTopWidth: 0, borderRightWidth: 0, borderBottomWidth: 0, borderLeftWidth: 0 })
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides!.fallback).toBe(0xf0)
})

test("widths native would reject are clamped", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(View, { borderTopWidth: -1, borderRightWidth: NaN, borderBottomWidth: 1e9 }))
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides!.widths).toEqual([0, 0, 4096, 0])
})
