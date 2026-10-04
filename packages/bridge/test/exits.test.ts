import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, Layer, Text, View, type KeyframeAnimation } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { ENV_BIT, EVENT_KIND, NIL } from "../src/wire.js"
import { animationIn, FILL } from "../src/motion.js"
import { readFrame, type Op } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(_: (seq: number) => void) {}
  event?: (ev: UiEvent) => void
  onEvent(cb: (ev: UiEvent) => void) { this.event = cb }
  close() {}
  /** The ops of the frames sent since the last call. */
  take(): Op[] {
    const ops = this.frames.flatMap(f => readFrame(f).ops)
    this.frames.length = 0
    return ops
  }
}

const CREATE = 0x01, DETACH = 0x03, REMOVE = 0x04, END_EXIT = 0x05, ANIMATION = 0xa3, KEYFRAMES = 0xa2
const tick = () => new Promise(r => setTimeout(r, 0))
const ev = (kind: number, node: number, key: number, generation = 0): UiEvent =>
  ({ kind, node, generation, revision: 0, x: 0, y: 0, a: 0, b: 0, key, text: "" })
const exitEnd = (t: FakeTransport, node: number, reason = 0, generation = 0) =>
  t.event!(ev(EVENT_KIND.exitEnd, node, reason, generation))
const reduced = (t: FakeTransport, on: boolean) =>
  t.event!(ev(EVENT_KIND.environment, NIL, on ? ENV_BIT.reducedMotion : 0))
const ids = (ops: Op[], tag: number) => ops.filter(o => o.tag === tag).map(o => o.id)

const enter: KeyframeAnimation = { keyframes: [{ at: 0, opacity: 0, translateY: 8 }], duration: 200 }
const fadeCollapse: KeyframeAnimation =
  { keyframes: [{ at: 1, opacity: 0, height: 0 }], duration: 200, reducedMotion: "fade" }

/** The target example: a toast list whose toasts fade in, and fade and
 * collapse out. */
function Toasts({ toasts, exit = fadeCollapse }: { toasts: string[]; exit?: KeyframeAnimation }) {
  return createElement(View, null, toasts.map(id =>
    createElement(View, { key: id, enter, exit },
      createElement(Text, null, `toast ${id}`))))
}

/** Renders `toasts`, returning the root and the ids of each toast's
 * view and text (in creation order). */
async function mount(toasts: string[], exit?: KeyframeAnimation) {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(createElement(Toasts, { toasts, exit }))
  await tick()
  const created = ids(t.take(), CREATE)
  return { t, root, created }
}

test("the example: the exit goes with the removal, fill forwards, with a height frame", async () => {
  const { t, root, created } = await mount(["a", "b"])
  const bView = created[3]
  // Nothing of it goes before.
  expect(created.length).toBe(5)
  root.renderSync(createElement(Toasts, { toasts: ["a"] }))
  await tick()
  const ops = t.take()
  // ANIMATION (trigger 3, no end events asked), then DETACH, of b.
  const at = ops.findIndex(o => o.tag === ANIMATION)
  const exit = ops[at]!
  expect([exit.id, exit.trigger, exit.notify]).toEqual([bView, 3, 0])
  expect([ops[at + 1]!.tag, ops[at + 1]!.id]).toEqual([DETACH, bView])
  const a = exit.animations![0]!
  expect([a.iterations, a.fill]).toEqual([1, 1])
  expect(a.duration).toBeCloseTo(0.2, 5)
  const frames = ops.filter(o => o.tag === KEYFRAMES)[a.keyframes]!.frames!
  // Opacity (16) and height (16384), at 1.
  expect(frames).toEqual([{ at: 1, easing: [0], mask: 16 | 16384, values: [0, 0] }])
})

test("a removed toast's ids stay parked until its exit ends, then recycle", async () => {
  const { t, root, created } = await mount(["a", "b"])
  const [, , , bView, bText] = created
  root.renderSync(createElement(Toasts, { toasts: ["a"] }))
  await tick()
  let ops = t.take()
  // React's removes do not reach native: it frees the subtree itself.
  expect(ids(ops, DETACH)).toEqual([bView])
  expect(ids(ops, REMOVE)).toEqual([])
  // A new toast meanwhile takes fresh ids.
  root.renderSync(createElement(Toasts, { toasts: ["a", "c"] }))
  await tick()
  const fresh = ids(t.take(), CREATE)
  expect(fresh.length).toBe(2)
  expect(fresh).not.toContain(bView)
  expect(fresh).not.toContain(bText)
  // A stale end (another generation) changes nothing; the real one frees.
  exitEnd(t, bView!, 0, 1)
  root.renderSync(createElement(Toasts, { toasts: ["a", "c", "d"] }))
  await tick()
  expect(ids(t.take(), CREATE)).not.toContain(bView)
  exitEnd(t, bView!)
  root.renderSync(createElement(Toasts, { toasts: ["a", "c", "d", "e"] }))
  await tick()
  ops = t.take()
  expect(ids(ops, CREATE).sort()).toEqual([bView!, bText!].sort())
})

test("events of an exiting subtree are dropped", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let pressed = 0
  const render = (show: boolean) => root.renderSync(createElement(View, null,
    show && createElement(View, { exit: fadeCollapse, onPointerDown: () => pressed++ })))
  render(true)
  await tick()
  const [, aView] = ids(t.take(), CREATE)
  t.event!(ev(EVENT_KIND.pointerDown, aView!, 0))
  expect(pressed).toBe(1)
  render(false)
  await tick()
  t.event!(ev(EVENT_KIND.pointerDown, aView!, 0))
  expect(pressed).toBe(1)
})

test("reduced motion: skip removes at once, fade keeps opacity, keep runs as declared", async () => {
  for (const reducedMotion of ["skip", "fade", "keep"] as const) {
    const exit = { ...fadeCollapse, reducedMotion }
    const t = new FakeTransport()
    const root = createRoot(t)
    root.renderSync(createElement(Toasts, { toasts: ["a"], exit }))
    await tick()
    reduced(t, true)
    t.take()
    root.renderSync(createElement(Toasts, { toasts: [], exit }))
    await tick()
    const ops = t.take()
    const sent = ops.filter(o => o.tag === ANIMATION && o.trigger === 3)
    const frames = ops.filter(o => o.tag === KEYFRAMES).map(o => o.frames![0]!)
    if (reducedMotion === "skip") {
      // A plain removal: the toast and its text go now.
      expect(sent).toEqual([])
      expect(ids(ops, REMOVE).length).toBe(2)
    } else {
      expect(frames[0]!.mask).toBe(reducedMotion === "fade" ? 16 : 16 | 16384)
      expect(ids(ops, REMOVE)).toEqual([])
    }
  }
})

test("unmounting the root ends every exit at once", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  // Toasts at the root level: each one's removal is a detach.
  const render = (toasts: string[]) => root.renderSync(toasts.map(id =>
    createElement(View, { key: id, exit: fadeCollapse }, createElement(Text, null, id))))
  render(["a", "b"])
  await tick()
  const [aView, aText, bView, bText] = ids(t.take(), CREATE)
  render(["a"])
  await tick()
  expect(ids(t.take(), DETACH)).toEqual([bView])
  root.unmount()
  await tick()
  const ops = t.take()
  // Toast a starts no exit: it goes now. The running exit of b is cut
  // short.
  expect(ops.filter(o => o.tag === ANIMATION)).toEqual([])
  expect(ids(ops, DETACH)).toEqual([aView])
  expect(ids(ops, REMOVE).sort()).toEqual([aView!, aText!].sort())
  expect(ids(ops, END_EXIT)).toEqual([bView])
  // Again: nothing more.
  root.unmount()
  await tick()
  expect(t.take()).toEqual([])
  // Native answers `removed` for b: its ids recycle.
  exitEnd(t, bView!, 3)
  render(["c", "d"])
  await tick()
  const fresh = ids(t.take(), CREATE)
  expect(fresh).toContain(bView!)
  expect(fresh).toContain(bText!)
})

test("an exit that ends as unmount cuts it recycles once", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const render = (toasts: string[]) => root.renderSync(toasts.map(id =>
    createElement(View, { key: id, exit: fadeCollapse }, createElement(Text, null, id))))
  render(["a"])
  await tick()
  const [aView, aText] = ids(t.take(), CREATE)
  render([])
  await tick()
  t.take()
  // Native finishes the exit and frees the subtree; its end is still on
  // its way when the app unmounts. `END_EXIT` (not `REMOVE`, which
  // native would reject for the freed id) is safe.
  root.unmount()
  await tick()
  const ops = t.take()
  expect(ids(ops, REMOVE)).toEqual([])
  expect(ids(ops, END_EXIT)).toEqual([aView])
  exitEnd(t, aView!, 0)
  // The later answer to END_EXIT never comes (native had nothing to
  // end); a stray repeat of the end is ignored. The ids recycle once.
  exitEnd(t, aView!, 0)
  render(["b", "c"])
  await tick()
  const fresh = ids(t.take(), CREATE)
  expect(fresh.filter(id => id === aView || id === aText).sort()).toEqual([aView!, aText!].sort())
})

test("an exit must end; only an exit sets a size", () => {
  const forever: KeyframeAnimation = { ...fadeCollapse, iterations: "infinite" }
  expect(() => animationIn(forever, FILL.forwards, false, true, 0, true)).toThrow(/must end/)
  expect(() => animationIn(fadeCollapse, FILL.none, false)).toThrow(/only an exit/)
})

test("a layer stays open while a child exits", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const render = (open: boolean) => root.renderSync(createElement(View, null,
    open && createElement(Layer, null, createElement(View, { exit: fadeCollapse }))))
  render(true)
  await tick()
  // A portal's children commit first: the layer, its child, then the
  // app's view.
  const [layer, child] = ids(t.take(), CREATE)
  render(false)
  await tick()
  let ops = t.take()
  expect(ids(ops, DETACH)).toEqual([child])
  expect(ids(ops, REMOVE)).toEqual([])
  exitEnd(t, child!)
  await tick()
  ops = t.take()
  expect(ids(ops, REMOVE)).toEqual([layer])
})

test("a node released after its exit ended recycles at once, with no remove", async () => {
  const t = new FakeTransport()
  const host = createRoot(t).host
  const n = host.node("view", { exit: fadeCollapse })
  host.place(null, n, null)
  await tick()
  t.take()
  host.detach(n)
  // Native's end arrives before React's release (passive effects).
  exitEnd(t, n.id)
  host.release(n)
  await tick()
  expect(ids(t.take(), REMOVE)).toEqual([])
  const m = host.node("view", {})
  host.place(null, m, null)
  expect(m.id).toBe(n.id)
  expect(m.gen).toBe(1)
})
