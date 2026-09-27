// Runtime vector shapes: SVG-like elements (`<Path>`, `<Circle>`, ...)
// under a `<Vector>`, flattened here into wire shapes. The strings (path
// data, points, transforms, dash arrays) go over as written; the native
// side parses and validates them.
//
//   <Vector viewBox="0 0 24 24" fill="none" stroke="#fff" strokeWidth={2}>
//     <Circle cx={12} cy={12} r={10} strokeDasharray="4 2" />
//     <Path d="m9 12 2 2 4-4" />
//   </Vector>

import { Fragment, isValidElement, type ReactNode } from "react"
import { color } from "./host.js"
import type { WireShape } from "./wire.js"

/** Paint and stroke attributes; on a `Vector` or `G` they are defaults
 * every shape inside inherits, as in SVG. */
export interface ShapeProps {
  /** A color, or "none". Default black. */
  fill?: string | number
  fillRule?: "nonzero" | "evenodd"
  /** A color, or "none" (the default). */
  stroke?: string | number
  strokeWidth?: number
  strokeLinecap?: "butt" | "round" | "square"
  strokeLinejoin?: "miter" | "round" | "bevel"
  strokeMiterlimit?: number
  /** Dash and gap lengths: "4 2", or [4, 2]. */
  strokeDasharray?: string | readonly number[]
  strokeDashoffset?: number
  opacity?: number
  /** An SVG transform list: "translate(4 0) rotate(45 12 12)". */
  transform?: string
}

type Pts = string | readonly number[]

export interface PathProps extends ShapeProps { d: string }
export interface CircleProps extends ShapeProps { cx?: number; cy?: number; r: number }
export interface EllipseProps extends ShapeProps { cx?: number; cy?: number; rx: number; ry: number }
export interface RectProps extends ShapeProps {
  x?: number; y?: number; width: number; height: number; rx?: number; ry?: number
}
export interface LineProps extends ShapeProps { x1?: number; y1?: number; x2?: number; y2?: number }
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

function paint(v: string | number | undefined, fallback: number): number {
  if (v === undefined) return fallback
  if (v === "none") return 0
  if (v === "currentColor") throw Error("currentColor is not supported yet: pass a color")
  return color(v)
}

/** An ellipse as two half arcs, starting at its rightmost point and
 * turning clockwise, like SVG (dashes start there). */
function ellipse(cx: number, cy: number, rx: number, ry: number): string {
  const a = `A${rx} ${ry} 0 1 1`
  return `M${cx + rx} ${cy}${a} ${cx - rx} ${cy}${a} ${cx + rx} ${cy}Z`
}

/** SVG's rect path: from (x + rx, y), clockwise, corners as arcs. */
function rect(p: RectProps): string {
  const { x = 0, y = 0, width: w, height: h } = p
  let rx = p.rx ?? p.ry ?? 0
  let ry = p.ry ?? p.rx ?? 0
  rx = Math.min(Math.max(rx, 0), w / 2)
  ry = Math.min(Math.max(ry, 0), h / 2)
  if (rx === 0 || ry === 0) return `M${x} ${y}H${x + w}V${y + h}H${x}Z`
  const a = `A${rx} ${ry} 0 0 1`
  return (
    `M${x + rx} ${y}H${x + w - rx}${a} ${x + w} ${y + ry}V${y + h - ry}` +
    `${a} ${x + w - rx} ${y + h}H${x + rx}${a} ${x} ${y + h - ry}V${y + ry}${a} ${x + rx} ${y}Z`
  )
}

const pts = (p: Pts) => (typeof p === "string" ? p : p.join(" "))

/** The shapes under a `Vector`, in paint order. Fragments, arrays and
 * `G` groups flatten; `G` attributes are inherited defaults, its
 * transform wraps its children's, and its opacity multiplies theirs.
 * Anything else (text, other components) is an error: shapes must be
 * direct elements. */
export function flattenShapes(children: ReactNode, inherited: ShapeProps = {}): WireShape[] {
  const out: WireShape[] = []
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
    p.opacity = (up.opacity ?? 1) * (own.opacity ?? 1)
    if (node.type === G) { walk(props.children, p); return }
    let kind = 0
    let geometry: string
    switch (node.type) {
      case Path: geometry = props.d; break
      case Circle: geometry = ellipse(props.cx ?? 0, props.cy ?? 0, props.r, props.r); break
      case Ellipse: geometry = ellipse(props.cx ?? 0, props.cy ?? 0, props.rx, props.ry); break
      case Rect: geometry = rect(props as RectProps); break
      case Line:
        geometry = `M${props.x1 ?? 0} ${props.y1 ?? 0}L${props.x2 ?? 0} ${props.y2 ?? 0}`
        break
      case Polyline: kind = 1; geometry = pts(props.points); break
      case Polygon: kind = 2; geometry = pts(props.points); break
      default:
        throw Error("Vector: children must be Path, Circle, Ellipse, Rect, Line, Polyline, Polygon or G")
    }
    const dashes = p.strokeDasharray
    out.push({
      kind,
      geometry: geometry ?? "",
      transform: p.transform ?? "",
      dashes: dashes === undefined ? "" : typeof dashes === "string" ? dashes : dashes.join(" "),
      fill: paint(p.fill, 0x0000_00ff),
      fillRule: RULE[p.fillRule ?? "nonzero"],
      stroke: paint(p.stroke, 0),
      strokeWidth: p.strokeWidth ?? 1,
      join: JOIN[p.strokeLinejoin ?? "miter"],
      cap: CAP[p.strokeLinecap ?? "butt"],
      miterLimit: p.strokeMiterlimit ?? 4,
      dashOffset: p.strokeDashoffset ?? 0,
      opacity: p.opacity ?? 1,
    })
  }
  walk(children, inherited)
  return out
}
