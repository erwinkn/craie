import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, View, type ViewProps } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { INTERACTION } from "../src/wire.js"
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

const INTERACTION_OP = 0x60
const tick = () => new Promise(r => setTimeout(r, 0))

test("aria-hidden and React Native's two props set A11Y_HIDDEN, and nothing else", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const flagsOf = async (props: ViewProps) => {
    root.renderSync(createElement(View, { focusable: true, ...props }))
    await tick()
    const op = t.take().find(o => o.tag === INTERACTION_OP)
    return op?.f[1]
  }
  expect(await flagsOf({ "aria-hidden": true })).toBe(INTERACTION.focusable | INTERACTION.a11yHidden)
  expect(await flagsOf({})).toBe(INTERACTION.focusable)
  expect(await flagsOf({ accessibilityElementsHidden: true })).toBe(INTERACTION.focusable | INTERACTION.a11yHidden)
  expect(await flagsOf({ importantForAccessibility: "no" })).toBe(INTERACTION.focusable)
  expect(await flagsOf({ importantForAccessibility: "no-hide-descendants" }))
    .toBe(INTERACTION.focusable | INTERACTION.a11yHidden)
  // Booleanish strings, and aria-hidden winning when set (React Native);
  // each render flips the flags, so each sends an op.
  expect(await flagsOf({ "aria-hidden": "false" })).toBe(INTERACTION.focusable)
  expect(await flagsOf({ "aria-hidden": "true" })).toBe(INTERACTION.focusable | INTERACTION.a11yHidden)
  expect(await flagsOf({ "aria-hidden": false, accessibilityElementsHidden: true })).toBe(INTERACTION.focusable)
  expect(await flagsOf({ accessibilityElementsHidden: true })).toBe(INTERACTION.focusable | INTERACTION.a11yHidden)
  expect(await flagsOf({ "aria-hidden": false, importantForAccessibility: "no-hide-descendants" }))
    .toBe(INTERACTION.focusable)
})
