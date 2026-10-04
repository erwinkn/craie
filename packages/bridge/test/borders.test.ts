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

test("a side's width or color sends all four sides, the rest from borderWidth and borderColor", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const render = (props: ViewProps) => root.renderSync(createElement(View, props))
  // The kit's divider: a bottom line.
  render({ borderBottomWidth: 1, borderColor: "#202020" })
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides).toEqual({
    widths: [0, 0, 1, 0],
    colors: [0x2020_20ff, 0x2020_20ff, 0x2020_20ff, 0x2020_20ff],
  })
  // One side's color over a uniform border.
  render({ borderWidth: 2, borderColor: "#202020", borderLeftColor: "#ff0000" })
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides).toEqual({
    widths: [2, 2, 2, 2],
    colors: [0x2020_20ff, 0x2020_20ff, 0x2020_20ff, 0xff00_00ff],
  })
  // The same sides again send none; dropping them sends zeros.
  render({ borderWidth: 2, borderColor: "#202020", borderLeftColor: "#ff0000" })
  await tick()
  expect(t.take().filter(o => o.tag === PAINT).length).toBe(0)
  render({ borderWidth: 2, borderColor: "#202020" })
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides).toEqual({ widths: [0, 0, 0, 0], colors: [0, 0, 0, 0] })
})

test("widths native would reject are clamped", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(View, { borderTopWidth: -1, borderRightWidth: NaN, borderBottomWidth: 1e9 }))
  await tick()
  expect(t.take().find(o => o.tag === PAINT)!.sides!.widths).toEqual([0, 0, 4096, 0])
})
