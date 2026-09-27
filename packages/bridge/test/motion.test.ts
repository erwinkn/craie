import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, View, type KeyframeAnimation, type Keyframe } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { Encoder, ENV_BIT, NIL } from "../src/wire.js"
import { animationIn } from "../src/motion.js"
import { readFrame, type Op } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_: (seq: number) => void) {}
  event?: (ev: UiEvent) => void
  onEvent(cb: (ev: UiEvent) => void) { this.event = cb }
  close() {}
  ops(i = -1) { return readFrame(this.frames.at(i)!).ops }
}

const tick = () => new Promise(r => setTimeout(r, 0))
const ev = (kind: number, node: number, key: number): UiEvent =>
  ({ kind, node, generation: 0, revision: 0, x: 0, y: 0, a: 0, b: 0, key, text: "" })
const reduced = (t: FakeTransport, on: boolean) => t.event!(ev(21, NIL, on ? ENV_BIT.reducedMotion : 0))
const tagged = (ops: Op[], tag: number) => ops.filter(o => o.tag === tag)
const close = (xs: number[], want: number[]) => {
  expect(xs.length).toBe(want.length)
  xs.forEach((x, i) => expect(x).toBeCloseTo(want[i]!, 5))
}

const spin: Keyframe[] = [{ at: 0, rotate: 0 }, { at: 1, rotate: 360 }]
const pulse: Keyframe[] = [{ at: 0.5, opacity: 0.4, scale: 0.95 }]
const loop = (keyframes: Keyframe[], duration: number, extra?: Partial<KeyframeAnimation>): KeyframeAnimation =>
  ({ keyframes, duration, easing: "linear", iterations: "infinite", ...extra })

test("the example: enter, a loop, a variant transition and a variant loop", async () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(View, {
    group: true,
    style: { opacity: 0.6 },
    enter: { keyframes: [{ at: 0, opacity: 0, translateY: 8 }], duration: 200, easing: [0.23, 1, 0.32, 1] },
    animation: loop(spin, 1000, { reducedMotion: "keep" }),
    _hover: { style: { scale: 1.02, transition: { scale: { duration: 120 } } } },
    _selected: { animation: { keyframes: pulse, duration: 400, iterations: "infinite" } },
  }))
  await tick()
  const ops = t.ops()
  // Keyframes go inline, each before its first use, one per content.
  const kf = tagged(ops, 0xa2)
  expect(kf.length).toBe(3)
  expect(ops.indexOf(kf[0]!)).toBeLessThan(ops.findIndex(o => o.tag === 0xa3))
  const [enter, anim] = tagged(ops, 0xa3)
  // Enter: trigger 0, fill backwards (2), seconds, the frame at 0 with
  // opacity (16) and translate y (512) as [points, fraction].
  expect([enter!.trigger, enter!.notify]).toEqual([0, 0])
  const e = enter!.animations![0]!
  expect([e.keyframes, e.iterations, e.direction, e.fill]).toEqual([0, 1, 0, 2])
  expect(e.duration).toBeCloseTo(0.2, 5)
  close(e.easing, [1, 0.23, 1, 0.32, 1])
  expect(kf[0]!.frames).toEqual([{ at: 0, easing: [0], mask: 16 | 512, values: [0, 8, 0] }])
  // The loop: trigger 1, infinite, fill none; rotate in radians.
  const a = anim!.animations![0]!
  expect([anim!.trigger, a.keyframes, a.iterations, a.fill]).toEqual([1, 1, Infinity, 0])
  close(kf[1]!.frames!.map(f => f.values[0]!), [0, 2 * Math.PI])
  // The variants: hover's scale with its transition (scale is prop 11),
  // selected's loop, scale x and y (2048 | 4096) at 0.5.
  const [hover, selected] = tagged(ops, 0xb1)[0]!.variants!
  expect(hover!.values[0]).toBe(2048 | 4096 | 8192)
  expect(hover!.transitions!.map(x => [x[0], x[1], x[3]])).toEqual([[11, 0, expect.closeTo(0.12, 5)]])
  expect(selected!.values[0]).toBe(16384)
  expect(selected!.animations![0]!.keyframes).toBe(2)
  expect(kf[2]!.frames![0]!.mask).toBe(16 | 2048 | 4096)
})

test("enter goes with the creation only; a list restarts on change only", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const enter = { keyframes: [{ at: 0, opacity: 0 }], duration: 150 }
  const render = (animation: KeyframeAnimation | false, width = 10) =>
    root.renderSync(createElement(View, { style: { width }, enter: { ...enter }, animation }))
  render(loop(spin, 1000))
  await tick()
  expect(tagged(t.ops(), 0xa3).map(o => o.trigger)).toEqual([0, 1])
  // A re-render with equal (fresh) objects sends nothing of either.
  render(loop([...spin], 1000), 20)
  await tick()
  expect(tagged(t.ops(), 0x10).length).toBe(1)
  expect(tagged(t.ops(), 0xa3)).toEqual([])
  // A changed list goes again; no list sends an empty one.
  render(loop(spin, 500))
  await tick()
  expect(tagged(t.ops(), 0xa3).map(o => o.animations![0]!.duration)).toEqual([0.5])
  render(false)
  await tick()
  expect(tagged(t.ops(), 0xa3).map(o => [o.trigger, o.animations!.length])).toEqual([[1, 0]])
  render(false)
  await tick()
  expect(tagged(t.ops(), 0xa3)).toEqual([])
})

test("easings: steps, linear points spread as CSS, springs", () => {
  const t = new FakeTransport()
  createRoot(t).renderSync(createElement(View, {
    animation: [
      { keyframes: [{ at: 1, opacity: 0 }], duration: 100, easing: { steps: 4, jump: "none" } },
      { keyframes: [{ at: 1, opacity: 0, easing: "ease-in" }], duration: 100, easing: { linear: [0, [0.25, 0.75], 0.5, 1] } },
      { keyframes: [{ at: 1, scale: 2 }], easing: { spring: { stiffness: 300 } } },
    ],
  }))
  return tick().then(() => {
    const [steps, linear, spring] = tagged(t.ops(), 0xa3)[0]!.animations!
    expect(steps!.easing).toEqual([2, 4, 2])
    // (input, output): the 0.5 between 0.75 and 1 lands at 0.875.
    close(linear!.easing, [3, 0, 0, 0.75, 0.25, 0.875, 0.5, 1, 1])
    expect(spring!.easing).toEqual([4, 300, 26, 1])
    // A frame's own easing covers the segment that starts at it.
    close(tagged(t.ops(), 0xa2)[1]!.frames![0]!.easing, [1, 0.42, 0, 1, 1])
    // Checked before a byte goes: offsets must rise, opacity in [0, 1].
    const enc = new Encoder()
    const bad = (a: KeyframeAnimation) => () => enc.animation(1, 1, false, [animationIn(a, 0, false)!])
    expect(bad({ keyframes: [{ at: 0.5, opacity: 0 }, { at: 0.2, opacity: 1 }], duration: 1 })).toThrow(/rise/)
    expect(bad({ keyframes: [{ at: 1, opacity: 2 }], duration: 1 })).toThrow(/opacity/)
    expect(bad({ keyframes: [{ at: 1, opacity: 0 }], duration: 1, easing: { steps: 1, jump: "none" } })).toThrow(/easing/)
    expect(bad({ keyframes: [{ at: 1, opacity: 0 }], iterations: "infinite" })).toThrow(/iterations/)
    expect(enc.empty).toBe(true)
  })
})

test("a Text animates no box paint", () => {
  const a = { keyframes: [{ at: 1, backgroundColor: "#fff" }], duration: 100 }
  expect(() => animationIn(a, 0, false, false)).toThrow(/box paint/)
  expect(animationIn(a, 0, false, true)!.frames[0]!.values).toEqual({ fill: 0xffffffff })
})

test("reduced motion: skip, fade and keep, followed live", async () => {
  const t = new FakeTransport()
  const fadeIn: Keyframe[] = [{ at: 0, opacity: 0, translateY: 8 }]
  createRoot(t).renderSync(createElement(View, {
    group: true,
    style: { transition: { opacity: { duration: 100, reducedMotion: "fade" }, scale: { duration: 100 } } },
    animation: [
      loop(spin, 1000),
      { keyframes: fadeIn, duration: 200, delay: 50, fill: "forwards" },
      { keyframes: fadeIn, duration: 200, reducedMotion: "fade" },
      loop(spin, 2000, { reducedMotion: "keep" }),
      loop(spin, 3000, { reducedMotion: "fade" }),
    ],
    _hover: { style: { transition: { scale: { duration: 50 } } }, animation: loop(pulse, 400) },
    onAnimationEnd: () => {},
  }))
  await tick()
  expect(tagged(t.ops(), 0xa3)[0]!.animations!.length).toBe(5)

  reduced(t, true)
  await tick()
  const ops = t.ops()
  // Transitions: skip drops scale; fade keeps opacity (prop 1).
  expect(tagged(ops, 0xa0)[0]!.f.filter((_, i) => i % 8 === 1)).toEqual([1])
  // The list: the skipped loop and the opacity-free fade loop go; the
  // finite one takes no time and keeps its fill; fade keeps opacity.
  const [list] = tagged(ops, 0xa3)
  const [skipped, faded, kept] = list!.animations!
  expect(list!.animations!.length).toBe(3)
  // Each keeps its index in the prop: the dropped loops move nothing.
  expect(list!.animations!.map(a => a.index)).toEqual([1, 2, 3])
  expect([skipped!.delay, skipped!.duration, skipped!.fill]).toEqual([0, 0, 1])
  expect(faded!.duration).toBeCloseTo(0.2, 5)
  expect(tagged(ops, 0xa2).find(k => k.frames![0]!.mask === 16)!.frames![0]!.values).toEqual([0])
  expect([kept!.iterations, kept!.duration]).toEqual([Infinity, 2])
  // The variant loses its loop, and its transition (skip): its list
  // is empty, and still replaces the node's while hover holds.
  const [hover] = tagged(ops, 0xb1)[0]!.variants!
  expect([hover!.transitions, hover!.animations]).toEqual([[], undefined])

  // Ends carry the prop's index: the fade is `animation[1]`, alone on
  // the wire.
  const got: unknown[] = []
  const t2 = new FakeTransport()
  createRoot(t2).renderSync(createElement(View, {
    animation: [loop(spin, 1000), { keyframes: fadeIn, duration: 100 }],
    onAnimationEnd: (e: { animation: string; index: number; reason: string }) => got.push([e.animation, e.index, e.reason]),
  }))
  await tick()
  reduced(t2, true)
  await tick()
  const [only] = tagged(t2.ops(), 0xa3)
  expect([only!.notify, only!.animations!.map(a => a.index)]).toEqual([1, [1]])
  t2.event!(ev(15, 0, 1 | (0 << 8) | (2 << 16)))
  expect(got).toEqual([["animation", 1, "finished"]])

  // Off again: everything goes back as declared.
  reduced(t, false)
  await tick()
  expect(tagged(t.ops(), 0xa3)[0]!.animations!.length).toBe(5)
  expect(tagged(t.ops(), 0xb1)[0]!.variants!.length).toBe(1)
})

test("keyframes name what they cannot animate", () => {
  const a = (k: Record<string, unknown>) => ({ keyframes: [{ at: 0, ...k } as Keyframe], duration: 100 })
  expect(() => animationIn(a({ bg: "red" }), 0, false)).toThrow(/"bg"/)
  expect(() => animationIn(a({ backgroundColour: "red" }), 0, false)).toThrow(/"backgroundColour"/)
  expect(() => animationIn(a({ width: 10 }), 0, false)).toThrow(/"width"/)
})

test("a negative delay starts partway, within 600 s", () => {
  const enc = new Encoder()
  const a = (delay: number) => animationIn({ keyframes: [{ at: 0, opacity: 0 }], duration: 1000, delay }, 0, false)!
  expect(a(-500).delay).toBeCloseTo(-0.5, 9)
  enc.animation(1, 1, false, [a(-500), { ...a(-600_000), index: 1 }])
  expect(() => enc.animation(1, 1, false, [a(-600_500)])).toThrow(/delay/)
  expect(() => enc.animation(1, 1, false, [a(0), a(0)])).toThrow(/indices/)
})

test("variants: blocks keep their positions, and a transition list replaces", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const render = (busy: boolean) =>
    root.renderSync(createElement(View, {
      group: true,
      style: { transition: { opacity: { duration: 100 } } },
      _pressed: busy ? { style: { opacity: 0.5 } } : {},
      _hover: { style: { scale: 1.02, transition: "none" } },
      _selected: { animation: loop(pulse, 400) },
    }))
  render(false)
  await tick()
  const sent = () => tagged(t.ops(), 0xb1)[0]!.variants!
  // The empty `_pressed` block goes unsent but keeps its position: the
  // loop is block 2 either way. `"none"` sends an empty list.
  let [hover, selected] = sent()
  expect([hover!.transitions, hover!.block, selected!.block]).toEqual([[], undefined, 2])
  render(true)
  await tick()
  expect(sent().map(v => v.block)).toEqual([undefined, undefined, 2])
})
