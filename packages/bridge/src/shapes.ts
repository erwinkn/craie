// Runtime vector shapes: SVG-like elements (`<Path>`, `<Circle>`, ...)
// under a `<Vector>`, flattened here into wire shapes. Numbers are
// coerced and checked here, as SVG treats them: a size of zero or less
// draws no shape, a number that is not finite drops its shape (warned
// once), an opacity clamps. The strings (path data, points, transforms)
// go over as written; the native side parses them, and a drawing with
// one that does not parse draws nothing.
//
// `currentColor` takes the nearest `color` on a `G` above the shape, as
// SVG does; without one it is left to native, which paints the node's
// inherited color (the Vector's own `color`, else an ancestor's, else
// white, as a span), so a Pressable's `_hover={{ color }}` recolors its
// icons with no JS.
//
//   <Vector viewBox="0 0 24 24" fill="none" stroke="#fff" strokeWidth={2}>
//     <Circle cx={12} cy={12} r={10} strokeDasharray="4 2" />
//     <Path d="m9 12 2 2 4-4" />
//   </Vector>

import { Fragment, isValidElement, type ReactNode } from "react"
import { color as parseColor, utf8Length, warnOnce } from "./host.js"
import { CURRENT, type WireShape } from "./wire.js"

/** Shapes one drawing may hold (native `svg::MAX_SHAPES`). */
export const MAX_SHAPES = 4096
/** Bytes a drawing's strings may add up to, a string shared by several
 * shapes counting once for each (native `svg::MAX_BYTES`, 4 MiB). */
export const MAX_BYTES = 4 << 20

type Num = number | string

/** Paint and stroke attributes; on a `Vector` or `G` they are defaults
 * every shape inside inherits, as in SVG. Numbers may be numeric
 * strings, as SVG attributes are. */
export interface ShapeProps {
  /** A color, "none", or "currentColor". Default black. */
  fill?: string | number
  /** Multiplies the fill color's alpha, 0 to 1. */
  fillOpacity?: Num
  fillRule?: "nonzero" | "evenodd"
  /** A color, "none" (the default), or "currentColor". */
  stroke?: string | number
  /** Multiplies the stroke color's alpha, 0 to 1. */
  strokeOpacity?: Num
  strokeWidth?: Num
  strokeLinecap?: "butt" | "round" | "square"
  strokeLinejoin?: "miter" | "round" | "bevel"
  strokeMiterlimit?: Num
  /** Dash and gap lengths: "4 2", [4, 2], or 4. */
  strokeDasharray?: Num | readonly Num[]
  strokeDashoffset?: Num
  /** What "currentColor" paints with. On a `G`, for the shapes inside
   * it. On `Vector`, the node's inherited color (as a Text's `color`):
   * variants and transitions apply to it, and without it the drawing
   * inherits its ancestors' color. */
  color?: string | number
  /** On a shape or `G`: multiplies into each shape's paint (overlapping
   * shapes in a faded `G` show through each other; DF-16). On `Vector`,
   * the node's opacity instead. */
  opacity?: Num
  /** An SVG transform list: "translate(4 0) rotate(45 12 12)". */
  transform?: string
}

type Pts = string | readonly Num[]

export interface PathProps extends ShapeProps { d: string }
export interface CircleProps extends ShapeProps { cx?: Num; cy?: Num; r: Num }
export interface EllipseProps extends ShapeProps { cx?: Num; cy?: Num; rx: Num; ry: Num }
export interface RectProps extends ShapeProps {
  x?: Num; y?: Num; width: Num; height: Num; rx?: Num; ry?: Num
}
export interface LineProps extends ShapeProps { x1?: Num; y1?: Num; x2?: Num; y2?: Num }
export interface PolyProps extends ShapeProps { points: Pts }
export interface GProps extends ShapeProps { children?: ReactNode }

// The shape elements only describe geometry: `Vector` reads them and
// never mounts them.
export function Path(_: PathProps): null { return null }
export function Circle(_: CircleProps): null { return null }
export function Ellipse(_: EllipseProps): null { return null }
export function Rect(_: RectProps): null { return null }
export function Line(_: LineProps): null { return null }
export function Polyline(_: PolyProps): null { return null }
export function Polygon(_: PolyProps): null { return null }
export function G(_: GProps): null { return null }

const RULE = { nonzero: 0, evenodd: 1 }
const JOIN = { miter: 0, round: 1, bevel: 2 }
const CAP = { butt: 0, round: 1, square: 2 }

/** `v` as a number (`Number`, as SVG reads attributes), or `fallback`
 * when absent. */
const num = (v: unknown, fallback: number): number =>
  v === undefined || v === null ? fallback : Number(v)

/** An opacity: clamped to 0..1, `fallback` when absent or NaN. */
function unit(v: unknown, fallback = 1): number {
  const n = num(v, fallback)
  return Number.isNaN(n) ? fallback : Math.min(Math.max(n, 0), 1)
}

/** `color`'s alpha scaled by `opacity` (0xRRGGBBAA). */
const fade = (c: number, opacity: number) =>
  opacity === 1 ? c : ((c & ~0xff) | Math.round((c & 0xff) * opacity)) >>> 0

/** A paint as [color, whether it is the node's inherited color]. An
 * unresolved `currentColor`'s color is the tint native multiplies the
 * inherited color by: white, with the opacity as alpha. */
function paint(
  v: string | number | undefined, fallback: number, p: ShapeProps, opacity: number,
): [number, boolean] {
  if (v === undefined) return [fade(fallback, opacity), false]
  if (v === "none") return [0, false]
  if (v === "currentColor") {
    if (p.color === undefined) return [fade(0xffff_ffff, opacity), true]
    return [fade(parseColor(p.color), opacity), false]
  }
  return [fade(parseColor(v), opacity), false]
}

/** A dash array as the native side reads it, or "" (solid) when a
 * length is negative or not a number, as SVG ignores such an array. */
function dashes(v: ShapeProps["strokeDasharray"]): string {
  if (v === undefined || v === "" || v === "none") return ""
  const parts = typeof v === "string" ? v.trim().split(/[\s,]+/) : typeof v === "object" ? v : [v]
  const ns = parts.map(Number)
  if (ns.every(n => Number.isFinite(n) && n >= 0)) return ns.join(" ")
  warnOnce(`Vector: strokeDasharray ${JSON.stringify(v)} is not a list of lengths; drawing solid`)
  return ""
}

/** An ellipse as two half arcs, starting at its rightmost point and
 * turning clockwise, like SVG (dashes start there). */
function ellipse(cx: number, cy: number, rx: number, ry: number): string {
  const a = `A${rx} ${ry} 0 1 1`
  return `M${cx + rx} ${cy}${a} ${cx - rx} ${cy}${a} ${cx + rx} ${cy}Z`
}

/** SVG's rect path: from (x + rx, y), clockwise, corners as arcs. */
function rect(x: number, y: number, w: number, h: number, rx: number, ry: number): string {
  rx = Math.min(Math.max(rx, 0), w / 2)
  ry = Math.min(Math.max(ry, 0), h / 2)
  if (rx === 0 || ry === 0) return `M${x} ${y}H${x + w}V${y + h}H${x}Z`
  const a = `A${rx} ${ry} 0 0 1`
  return (
    `M${x + rx} ${y}H${x + w - rx}${a} ${x + w} ${y + ry}V${y + h - ry}` +
    `${a} ${x + w - rx} ${y + h}H${x + rx}${a} ${x} ${y + h - ry}V${y + ry}${a} ${x + rx} ${y}Z`
  )
}

const NAMES = "Path, Circle, Ellipse, Rect, Line, Polyline, Polygon or G"

/** A shape element's kind and geometry string, or null when it draws
 * nothing: a size of zero or less (as in SVG), or a number that is not
 * finite (`<Circle r={NaN}>`, a chart's missing point), warned once. */
function geometry(type: unknown, props: Record<string, any>): [number, string] | null {
  const bad = (name: string) => {
    warnOnce(`Vector: a ${name} with a number that is not finite is not drawn`)
    return null
  }
  const ok = (...ns: number[]) => ns.every(Number.isFinite)
  switch (type) {
    case Path: return [0, typeof props.d === "string" ? props.d : ""]
    case Circle:
    case Ellipse: {
      const cx = num(props.cx, 0), cy = num(props.cy, 0)
      const rx = num(type === Circle ? props.r : props.rx, 0)
      const ry = num(type === Circle ? props.r : props.ry, 0)
      if (!ok(cx, cy, rx, ry)) return bad(type === Circle ? "Circle" : "Ellipse")
      return rx > 0 && ry > 0 ? [0, ellipse(cx, cy, rx, ry)] : null
    }
    case Rect: {
      const x = num(props.x, 0), y = num(props.y, 0)
      const w = num(props.width, 0), h = num(props.height, 0)
      // SVG: a missing corner radius takes the other's.
      const rx = num(props.rx ?? props.ry, 0), ry = num(props.ry ?? props.rx, 0)
      if (!ok(x, y, w, h, rx, ry)) return bad("Rect")
      return w > 0 && h > 0 ? [0, rect(x, y, w, h, rx, ry)] : null
    }
    case Line: {
      const [x1, y1, x2, y2] = [props.x1, props.y1, props.x2, props.y2].map(v => num(v, 0))
      if (!ok(x1!, y1!, x2!, y2!)) return bad("Line")
      return [0, `M${x1} ${y1}L${x2} ${y2}`]
    }
    case Polyline:
    case Polygon: {
      const kind = type === Polyline ? 1 : 2
      const p = props.points
      if (typeof p === "string") return [kind, p]
      const ns: number[] = Array.isArray(p) ? p.map(Number) : []
      if (!ok(...ns)) return bad(kind === 1 ? "Polyline" : "Polygon")
      // SVG drops an odd trailing number.
      return [kind, ns.slice(0, ns.length & ~1).join(" ")]
    }
    default:
      throw Error(`Vector: children must be ${NAMES}`)
  }
}

/** The shapes under a `Vector`, in paint order. Fragments, arrays and
 * `G` groups flatten; `G` attributes are inherited defaults, its
 * transform wraps its children's, and its opacity multiplies theirs.
 * Anything else (text, other components) is an error: shapes must be
 * direct elements. So is a drawing past `MAX_SHAPES` shapes or
 * `MAX_BYTES` of strings, which the native side would reject. */
export function flattenShapes(
  children: ReactNode, inherited: ShapeProps = {}, viewBox = "",
): WireShape[] {
  const out: WireShape[] = []
  let chars = viewBox.length
  const walk = (node: ReactNode, up: ShapeProps) => {
    if (node === null || node === undefined || typeof node === "boolean") return
    if (Array.isArray(node)) { for (const c of node) walk(c, up); return }
    if (!isValidElement(node)) throw Error(`Vector: unexpected child ${String(node)}`)
    const props = node.props as Record<string, any>
    if (node.type === Fragment) { walk(props.children, up); return }
    // Attributes cascade; the transform nests and the opacity multiplies.
    const own = props as ShapeProps
    const p: ShapeProps & Record<string, any> = { ...up }
    for (const k in own) if ((own as any)[k] !== undefined) (p as any)[k] = (own as any)[k]
    p.transform = [up.transform, own.transform].filter(Boolean).join(" ")
    p.opacity = unit(up.opacity) * unit(own.opacity)
    if (node.type === G) { walk(props.children, p); return }
    const g = geometry(node.type, props)
    if (g === null) return
    if (out.length === MAX_SHAPES) {
      throw Error(
        `Vector: more than ${MAX_SHAPES} shapes; one drawing holds at most that many ` +
        "(split it across several Vectors)")
    }
    const width = num(p.strokeWidth, 1)
    const miter = num(p.strokeMiterlimit, 4)
    const offset = num(p.strokeDashoffset, 0)
    const [fill, fillCurrent] = paint(p.fill, 0x0000_00ff, p, unit(p.fillOpacity))
    const [stroke, strokeCurrent] = paint(p.stroke, 0, p, unit(p.strokeOpacity))
    const shape: WireShape = {
      kind: g[0],
      geometry: g[1],
      transform: p.transform,
      dashes: dashes(p.strokeDasharray),
      fill,
      fillRule: RULE[p.fillRule ?? "nonzero"],
      stroke,
      current: (fillCurrent ? CURRENT.fill : 0) | (strokeCurrent ? CURRENT.stroke : 0),
      // SVG: a negative width or a miter limit under 1 is an error,
      // and the attribute takes its default.
      strokeWidth: Number.isFinite(width) && width >= 0 ? width : 1,
      join: JOIN[p.strokeLinejoin ?? "miter"],
      cap: CAP[p.strokeLinecap ?? "butt"],
      miterLimit: Number.isFinite(miter) && miter >= 1 ? miter : 4,
      dashOffset: Number.isFinite(offset) ? offset : 0,
      opacity: p.opacity,
    }
    chars += shape.geometry.length + shape.transform.length + shape.dashes.length
    out.push(shape)
  }
  walk(children, inherited)
  // UTF-16 units undercount UTF-8 bytes at most threefold: count exactly
  // only near the limit.
  if (chars * 3 > MAX_BYTES) {
    let bytes = utf8Length(viewBox)
    for (const s of out) bytes += utf8Length(s.geometry) + utf8Length(s.transform) + utf8Length(s.dashes)
    if (bytes > MAX_BYTES) {
      throw Error(
        `Vector: ${bytes} bytes of path data, points and transforms; one drawing holds ` +
        `at most ${MAX_BYTES} (a string shared by several shapes counts for each)`)
    }
  }
  return out
}
