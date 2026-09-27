import { test, expect } from "bun:test"
import { createElement, useState } from "react"
import { createRoot, FocusGroup, Pressable } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { Encoder, GROUP, ROLE } from "../src/wire.js"
import { readFrame } from "./crw2.js"
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
const ROLE_OP = 0x50, GROUP_OP = 0x63

test("the group op is id u32 | flags u8", () => {
  const enc = new Encoder()
  enc.group(5, GROUP.vertical | GROUP.selectOnFocus)
  const [op] = readFrame(enc.finish(1n)).ops
  expect(op).toMatchObject({ tag: GROUP_OP, id: 5, f: [GROUP.vertical | GROUP.selectOnFocus] })
  expect(GROUP).toEqual({ horizontal: 1, vertical: 2, loop: 4, selectOnFocus: 8 })
})

test("a FocusGroup sends its flags: both orientations and loop by default", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(
    createElement(FocusGroup, null, createElement(Pressable, { onPress() {} })),
  )
  await tick()
  const groups = t.all().filter(o => o.tag === GROUP_OP)
  expect(groups.map(o => o.f)).toEqual([[GROUP.horizontal | GROUP.vertical | GROUP.loop]])
})

// The example: a vertical radiogroup with selectOnFocus. Its role and
// the radios' roles are the usual role ops; the flags follow the props.
test("the radiogroup example, and flags that change", async () => {
  const t = new FakeTransport()
  let set!: (p: { orientation?: "horizontal" | "vertical" | "both"; loop?: boolean }) => void
  function Sizes() {
    const [p, s] = useState<{ orientation?: "horizontal" | "vertical" | "both"; loop?: boolean }>({
      orientation: "vertical",
    })
    set = s
    const [size, setSize] = useState("m")
    return createElement(FocusGroup, { accessibilityRole: "radiogroup", selectOnFocus: true, ...p },
      createElement(Pressable, { accessibilityRole: "radio", checked: size === "s", onPress: () => setSize("s") }),
      createElement(Pressable, { accessibilityRole: "radio", checked: size === "m", onPress: () => setSize("m") }),
      createElement(Pressable, { accessibilityRole: "radio", disabled: true }))
  }
  createRoot(t).renderSync(createElement(Sizes))
  await tick()
  const ops = t.all()
  const [group] = ops.filter(o => o.tag === GROUP_OP)
  expect(group!.f).toEqual([GROUP.vertical | GROUP.loop | GROUP.selectOnFocus])
  const roles = ops.filter(o => o.tag === ROLE_OP)
  expect(roles.find(o => o.id === group!.id)!.f[0]).toBe(ROLE.radiogroup)
  expect(roles.filter(o => o.f[0] === ROLE.radio)).toHaveLength(3)

  t.frames.length = 0
  set({ orientation: "horizontal", loop: false })
  await settle(t.sent)
  expect(t.all().filter(o => o.tag === GROUP_OP)).toMatchObject([
    { id: group!.id, f: [GROUP.horizontal | GROUP.selectOnFocus] },
  ])
  // An unchanged group sends nothing again.
  t.frames.length = 0
  set({ orientation: "horizontal", loop: false })
  await tick()
  expect(t.all().filter(o => o.tag === GROUP_OP)).toEqual([])
})

test("tab and tablist are roles 19 and 20", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(
    createElement(FocusGroup, { accessibilityRole: "tablist", orientation: "horizontal" },
      createElement(Pressable, { accessibilityRole: "tab", selected: true, onPress() {} })),
  )
  await tick()
  const roles = t.all().filter(o => o.tag === ROLE_OP).map(o => o.f[0])
  expect(roles).toContain(20)
  expect(roles).toContain(19)
})
