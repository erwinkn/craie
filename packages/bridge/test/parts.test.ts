// Transform parts: the style keys (translate, rotate, scale, their axes)
// beside `transform`, what the spatial op carries per change, variants
// and transitions per part, `animate` targets, and the encoding.
import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, Pressable, View } from "../src/index.js"
import type { HostNode, Transport, UiEvent } from "../src/host.js"
import { ANIM_PROP, Encoder, layoutPart, partsOf, styleParts, type StyleProps } from "../src/wire.js"
import { readFrame } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_: (seq: number) => void) {}
  onEvent(_: (ev: UiEvent) => void) {}
  close() {}
  ops(i = -1) { return readFrame(this.frames.at(i)!).ops }
}

const tick = () => new Promise(r => setTimeout(r, 0))
const r4 = (v: number) => Math.round(v * 1e4) / 1e4
const DEG = Math.PI / 180
// SPATIAL mask bits.
const MATRIX = 1, OPACITY = 2, Z = 4, TRANSLATE = 8, ROTATE = 16, SCALE = 32

test("the spatial op carries each part in mask order", () => {
  const enc = new Encoder()
  enc.spatial(3, {
    transform: [1, 0, 0.5, 1, 0, 0], opacity: 0.5, z: -1,
    translate: [4, -2, 0.5, 0.25], rotate: 1.5, scale: [2, 0.5],
  })
  enc.spatial(4, { rotate: -1 })
  const [all, one] = readFrame(enc.finish(1n)).ops
  expect(all!.f).toEqual([0x3f, 1, 0, 0.5, 1, 0, 0, 0.5, -1, 4, -2, 0.5, 0.25, 1.5, 2, 0.5])
  expect(one!.f).toEqual([ROTATE, -1])
})

test("style keys resolve per axis over translate and scale", () => {
  expect(partsOf({ translate: ["50%", 10], rotate: 90, scale: 2 })).toEqual({
    translate: [0, 10, 0.5, 0], rotate: 90 * DEG, scale: [2, 2], matrix: [1, 0, 0, 1, 0, 0],
  })
  // One translate value moves x alone (CSS); an axis key overrides one.
  expect(partsOf({ translate: 10, translateY: "25%", scale: [2, 3], scaleY: 4 })).toMatchObject({
    translate: [10, 0, 0, 0.25], scale: [2, 4],
  })
  expect(partsOf({ rotate: "0.5rad" }).rotate).toBe(0.5)
  expect(r4(partsOf({ rotate: "0.25turn" }).rotate)).toBe(r4(Math.PI / 2))
  // A style sets only what it names: a variant overrides those.
  expect(styleParts({ translateX: "-50%", scaleY: 0.5 })).toEqual({ translateX: [0, -0.5], scaleY: 0.5 })
  expect(r4(partsOf({ rotate: "100grad" }).rotate)).toBe(r4(Math.PI / 2))
  expect(() => partsOf({ rotate: "12gon" as any })).toThrow()
  expect(() => partsOf({ rotate: "deg" as any })).toThrow()
  expect(() => partsOf({ translate: "1em" as any })).toThrow()
  expect(() => partsOf({ scale: [1] as any })).toThrow()
  // Parts are spatial: none of them reaches layout.
  const parts: StyleProps = { translate: 1, translateX: 1, translateY: 1, rotate: 1, scale: 1, scaleX: 1, scaleY: 1 }
  expect(layoutPart(parts)).toBeUndefined()
  expect(layoutPart({ ...parts, width: 3 })).toEqual({ width: 3 })
})

test("a commit sends only the parts that changed", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const view = (style: StyleProps) => createElement(View, { style: { width: 10, ...style } })
  root.renderSync(view({ translate: ["50%", 10], rotate: 90, scale: 2, transform: [{ translateX: 3 }] }))
  await tick()
  const first = t.ops(0).find(o => o.tag === 0x20)!
  expect(first.f.map(r4)).toEqual([
    MATRIX | TRANSLATE | ROTATE | SCALE, 1, 0, 0, 1, 3, 0, 0, 10, 0.5, 0, r4(Math.PI / 2), 2, 2,
  ])
  t.frames.length = 0
  root.renderSync(view({ translate: ["50%", 10], rotate: 45, scale: 2, transform: [{ translateX: 3 }], opacity: 0.5 }))
  await tick()
  expect(t.ops().map(o => [o.tag, ...o.f.map(r4)])).toEqual([[0x20, ROTATE | OPACITY, 0.5, r4(Math.PI / 4)]])
  // Dropping every part restores each to its identity; z rides along.
  t.frames.length = 0
  root.renderSync(view({ opacity: 0.5, zIndex: 2 }))
  await tick()
  expect(t.ops().map(o => [o.tag, ...o.f.map(r4)])).toEqual([
    [0x20, MATRIX | Z | TRANSLATE | ROTATE | SCALE, 1, 0, 0, 1, 0, 0, 2, 0, 0, 0, 0, 0, 1, 1],
  ])
  // Parts never relayout: no layout op went out.
  expect(t.ops().some(o => o.tag === 0x10)).toBe(false)
})

test("the Pressable example: a variant scale, its own transition, the base rotate", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(
    createElement(Pressable, {
      style: { rotate: "12deg", transition: { scale: { duration: 120 } } },
      _hover: { style: { scale: 1.02 } },
      _pressed: { style: { scale: 0.98, translateY: "10%" } },
    }),
  )
  await tick()
  const ops = t.ops()
  // The base: rotate alone.
  expect(ops.find(o => o.tag === 0x20)!.f.map(r4)).toEqual([ROTATE, r4(12 * DEG)])
  // The transition: scale (11) alone, 0.12 s.
  const tr = ops.find(o => o.tag === 0xa0)!
  expect([tr.f[0], tr.f[1], r4(tr.f[4]!)]).toEqual([1, ANIM_PROP.scale, 0.12])
  // The variants: scale x and y; the pressed one adds translate y as
  // [points, fraction].
  const [hover, pressed] = ops.find(o => o.tag === 0xb1)!.variants!
  const SCALE_XY = (1 << 11) | (1 << 12)
  expect(hover!.values.map(r4)).toEqual([SCALE_XY, 1.02, 1.02])
  expect(pressed!.values.map(r4)).toEqual([SCALE_XY | (1 << 9), 0, 0.1, 0.98, 0.98])
})

test("animate takes a target per part", async () => {
  const t = new FakeTransport()
  let node: HostNode | null = null
  createRoot(t).renderSync(createElement(View, { ref: (n: HostNode | null) => { node = n } }))
  await tick()
  t.frames.length = 0
  const n = node as unknown as HostNode
  n.animate("rotate", 360, { duration: 100 })
  n.animate("translate", ["50%", 4], { duration: 100 })
  n.animate("scale", 2, { duration: 100 })
  n.animate("scale", [1, 0.5], { duration: 100 })
  await tick()
  const ops = t.ops()
  expect(ops.map(o => o.tag)).toEqual([0xa1, 0xa1, 0xa1, 0xa1])
  expect(ops.map(o => o.f.slice(0, -7).map(r4))).toEqual([
    [ANIM_PROP.rotate, r4(2 * Math.PI)],
    [ANIM_PROP.translate, 0, 4, 0.5, 0],
    [ANIM_PROP.scale, 2, 2],
    [ANIM_PROP.scale, 1, 0.5],
  ])
  await expect(n.animate("scale", "big", { duration: 1 })).rejects.toThrow()
  await expect(n.animate("rotate", "1gon", { duration: 1 })).rejects.toThrow()
})
