// Keyframe animations and transitions from props to wire form, with the
// reduced-motion policy applied. Native runs what this sends; the policy
// resolves here, so a setting change re-sends (host.ts).

import { color } from "./host.js"
import {
  EASING,
  styleParts,
  type AnimationEasing,
  type AnimationIn,
  type AnimProp,
  type EasingIn,
  type FrameValues,
  type Keyframe,
  type KeyframeAnimation,
  type Transitions,
} from "./wire.js"

/** What `enter` and `animation` take: one animation, a list (falsy
 * entries skip, keeping the others' indices), or nothing. An entry is
 * its index: entries coming and going around it leave it running. */
export type Animations =
  | KeyframeAnimation
  | readonly (KeyframeAnimation | false | null | undefined)[]
  | false
  | null
  | undefined

// keyframes.rs `Direction`, `Fill`, `jump`.
const DIRECTION = { normal: 0, reverse: 1, alternate: 2, "alternate-reverse": 3 } as const
export const FILL = { none: 0, forwards: 1, backwards: 2, both: 3 } as const
const JUMP = { start: 0, end: 1, none: 2, both: 3 } as const

const LINEAR: EasingIn = [1, ...EASING.linear]

/** An easing in wire form. */
export function easingIn(e: AnimationEasing): EasingIn {
  if (typeof e === "string") {
    const b = Object.hasOwn(EASING, e) ? EASING[e] : undefined
    if (!b) throw Error(`unknown easing "${e}"`)
    return [1, ...b]
  }
  if (Array.isArray(e)) return [1, ...(e as readonly number[])]
  if ("steps" in e) {
    const jump = JUMP[e.jump ?? "end"]
    if (jump === undefined) throw Error(`unknown steps jump "${String(e.jump)}"`)
    return [2, e.steps, jump]
  }
  if ("linear" in e) return [3, ...linearPoints(e.linear)]
  if ("spring" in e) return [4, e.spring.stiffness ?? 170, e.spring.damping ?? 26, e.spring.mass ?? 1]
  throw Error(`unknown easing ${JSON.stringify(e)}`)
}

/** CSS `linear()` points as (input, output) pairs: the first input
 * defaults to 0 and the last to 1, an input below an earlier one rises
 * to it, and the rest spread evenly between their neighbours.
 * [0, [0.25, 0.75], 1] is (0, 0), (0.75, 0.25), (1, 1). */
function linearPoints(points: readonly (number | readonly [number, number])[]): number[] {
  const out = points.map(p => (typeof p === "number" ? p : p[0]))
  const inp = points.map(p => (typeof p === "number" ? NaN : p[1]))
  if (inp.length) {
    if (Number.isNaN(inp[0])) inp[0] = 0
    if (Number.isNaN(inp.at(-1))) inp[inp.length - 1] = Math.max(1, ...inp.filter(x => !Number.isNaN(x)))
  }
  let high = -Infinity
  for (let i = 0; i < inp.length; i++) if (!Number.isNaN(inp[i])) inp[i] = high = Math.max(high, inp[i]!)
  for (let i = 1; i < inp.length; i++) {
    if (!Number.isNaN(inp[i])) continue
    let j = i
    while (Number.isNaN(inp[j])) j++
    const a = inp[i - 1]!, b = inp[j]!
    for (let k = i; k < j; k++) inp[k] = a + ((b - a) * (k - i + 1)) / (j - i + 1)
  }
  return out.flatMap((o, i) => [inp[i]!, o])
}

const KEYFRAME_KEYS = new Set([
  "at", "easing", "opacity", "translate", "translateX", "translateY", "rotate",
  "scale", "scaleX", "scaleY", "backgroundColor", "borderColor", "color",
])

/** A keyframe's values in wire form; `boxed`: the node has a box (not
 * a Text), so background and border colors apply. An unknown key (a
 * typo, a property keyframes cannot animate) throws. */
function frameValues(k: Keyframe, boxed: boolean): FrameValues {
  for (const key in k) if (!KEYFRAME_KEYS.has(key)) throw Error(`keyframes cannot animate "${key}"`)
  const v: FrameValues = {}
  if (k.backgroundColor !== undefined || k.borderColor !== undefined) {
    if (!boxed) throw Error("a Text animates no box paint: wrap it in a View")
    if (k.backgroundColor !== undefined) v.fill = color(k.backgroundColor)
    if (k.borderColor !== undefined) v.borderColor = color(k.borderColor)
  }
  if (k.color !== undefined) v.color = color(k.color)
  if (k.opacity !== undefined) v.opacity = k.opacity
  const p = styleParts({
    translate: k.translate, translateX: k.translateX, translateY: k.translateY,
    rotate: k.rotate, scale: k.scale, scaleX: k.scaleX, scaleY: k.scaleY,
  })
  if (p.translateX) v.translateX = p.translateX
  if (p.translateY) v.translateY = p.translateY
  if (p.rotate !== undefined) v.rotate = p.rotate
  if (p.scaleX !== undefined) v.scaleX = p.scaleX
  if (p.scaleY !== undefined) v.scaleY = p.scaleY
  return v
}

/** One animation in wire form under the reduced-motion setting
 * (`reduced`), `index` its position in the author's list, or null when
 * the policy drops it:
 * - `skip`: a loop does not start; a finite animation takes no time,
 *   so it ends at once and its fill (`forwards`) holds the end;
 * - `fade`: only opacity frames go (none: as `skip`);
 * - `keep`: as declared. */
export function animationIn(
  a: KeyframeAnimation,
  defaultFill: number,
  reduced: boolean,
  boxed = true,
  index = 0,
): AnimationIn | null {
  let frames = a.keyframes.map(k => ({
    at: k.at,
    ...(k.easing !== undefined && { easing: easingIn(k.easing) }),
    values: frameValues(k, boxed),
  }))
  let policy = reduced ? a.reducedMotion ?? "skip" : "keep"
  if (policy === "fade") {
    frames = frames
      .filter(f => f.values.opacity !== undefined)
      .map(f => ({ ...f, values: { opacity: f.values.opacity } }))
    if (!frames.length) policy = "skip"
  }
  const iterations = a.iterations === "infinite" ? Infinity : a.iterations ?? 1
  const out: AnimationIn = {
    index,
    frames,
    delay: ms(a.delay ?? 0),
    duration: ms(a.duration ?? 0),
    easing: easingIn(a.easing ?? "ease"),
    iterations,
    direction: DIRECTION[a.direction ?? "normal"] ?? -1,
    fill: a.fill !== undefined ? FILL[a.fill] ?? -1 : defaultFill,
  }
  if (policy === "skip") {
    if (iterations === Infinity) return null
    // A spring would set its own duration: skip it too.
    return { ...out, delay: 0, duration: 0, easing: out.easing[0] === 4 ? LINEAR : out.easing }
  }
  return out
}

function ms(v: number): number {
  if (typeof v !== "number") throw Error(`bad animation time ${String(v)}`)
  return v / 1000
}

/** A prop's animations in wire form, each carrying its index in the
 * prop: native keys it by that, so a falsy or dropped entry leaves the
 * others running, and ends report it. */
export function animationList(
  v: Animations,
  defaultFill: number,
  reduced: boolean,
  boxed = true,
): AnimationIn[] {
  const all = !v ? [] : Array.isArray(v) ? v : [v as KeyframeAnimation]
  const list: AnimationIn[] = []
  all.forEach((a, i) => {
    const w = a ? animationIn(a, defaultFill, reduced, boxed, i) : null
    if (w) list.push(w)
  })
  return list
}

/** Transitions under the reduced-motion setting: `skip` (the default)
 * drops a property's timing (its changes jump), `fade` keeps opacity's
 * alone, `keep` keeps it. `"none"` is the empty list. */
export function transitionsIn(t: Transitions | "none" | undefined, reduced: boolean): Transitions | undefined {
  if (t === "none") return {}
  if (!t || !reduced) return t
  const out: Transitions = {}
  for (const k in t) {
    const p = k as AnimProp
    const x = t[p]
    const policy = x?.reducedMotion ?? "skip"
    if (x && (policy === "keep" || (policy === "fade" && p === "opacity"))) out[p] = x
  }
  return out
}
