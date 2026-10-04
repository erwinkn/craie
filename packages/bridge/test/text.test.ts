import { test, expect } from "bun:test"
import { createElement as h } from "react"
import { createRoot, Text } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { readFrame } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_: (seq: number) => void) {}
  onEvent(_: (ev: UiEvent) => void) {}
  close() {}
}

const tick = () => new Promise(r => setTimeout(r, 0))

test("textAlign is the paragraph's; fontVariant's tabular-nums is per span", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(h(Text, { textAlign: "center", fontVariant: ["tabular-nums"] },
    "Total ",
    h(Text, { fontVariant: ["proportional-nums"], textAlign: "right" }, "12"),
    " of 99"))
  await tick()
  const spans = t.frames.map(f => readFrame(f)).find(f => f.spans.length)!.spans
  // Span zero and the run after the nested Text inherit the root's; the
  // nested Text sets its own tabular, never the alignment.
  expect(spans.map(s => [s.tabular, s.align])).toEqual([[true, 2], [false, 2], [true, 2]])
})

test("an unknown alignment draws as auto", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(h(Text, { textAlign: "justify" }, "x"))
  await tick()
  const spans = t.frames.map(f => readFrame(f)).find(f => f.spans.length)!.spans
  expect(spans[0]!.align).toBe(0)
})

test("numberOfLines sends the outermost Text's line limit when it changes", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const ops = () => t.frames.splice(0).flatMap(f => readFrame(f).ops).filter(o => o.tag === 0x42)
  root.renderSync(h(Text, { numberOfLines: 1 }, "a long title"))
  await tick()
  expect(ops().map(o => o.f[0])).toEqual([1])
  root.renderSync(h(Text, { numberOfLines: 1 }, "another title"))
  await tick()
  expect(ops()).toEqual([])
  root.renderSync(h(Text, {}, "another title"))
  await tick()
  expect(ops().map(o => o.f[0])).toEqual([0])
})
