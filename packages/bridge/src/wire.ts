// Binary transaction encoder — the exact mirror of crates/craie/src/wire.rs.
//
//   header: magic "CRW1" u32 | version u16 | flags u16 | seq u64 | strings u32
//   strings: count x (u32 byte_len + utf8)
//   ops:     flat u8-tagged records to end of buffer
//
// Style ops carry a u64 presence mask + positional fields (see FIELD).
// Strings are interned per transaction and referenced by index.

const MAGIC = 0x3157_5243 // "CRW1" little-endian
const VERSION = 1
export const NIL = 0xffff_ffff // no node / append / clear

const enum Op {
  Create = 0x01,
  SetText = 0x02,
  TextProps = 0x03,
  SetStyle = 0x04,
  Place = 0x05,
  Detach = 0x06,
  Remove = 0x07,
  Hidden = 0x08,
  Style = 0x09,
  ViewPaint = 0x0a,
}

// Style schema — mask bit order must match wire.rs `mod field`.
const F = {
  DISPLAY: 1 << 0,
  POSITION: 1 << 1,
  FLEX_DIRECTION: 1 << 2,
  FLEX_WRAP: 1 << 3,
  JUSTIFY_CONTENT: 1 << 4,
  ALIGN_ITEMS: 1 << 5,
  ALIGN_CONTENT: 1 << 6,
  ALIGN_SELF: 1 << 7,
  GAP: 1 << 8,
  SIZE: 1 << 9,
  MIN_SIZE: 1 << 10,
  MAX_SIZE: 1 << 11,
  PADDING: 1 << 12,
  MARGIN: 1 << 13,
  BORDER: 1 << 14,
  INSET: 1 << 15,
  FLEX_BASIS: 1 << 16,
  FLEX_GROW: 1 << 17,
  FLEX_SHRINK: 1 << 18,
  ASPECT_RATIO: 1 << 19,
  OVERFLOW: 1 << 20,
} as const

// Alignment keyword tags — mirror wire.rs `mod kw`.
const KW: Record<string, number> = {
  start: 0, end: 1, "flex-start": 2, "flex-end": 3, "self-start": 4,
  "self-end": 5, center: 6, baseline: 7, stretch: 8,
  "space-between": 9, "space-evenly": 10, "space-around": 11,
}
const UNSET = 0xff

// Dimension encodings. LP: 0 len, 1 pct. LPA adds 2 auto. Dimension adds
// 3 min-content, 4 max-content, 5 fit f32, 6 fit% f32, 7 fit, 8 stretch, 9 content.
export type Dimension =
  | number | `${number}%` | "auto" | "min-content" | "max-content"
  | "fit-content" | "stretch" | "content"
export type LengthPct = number | `${number}%`
export type LengthPctAuto = LengthPct | "auto"
export type Edges<T> = T | { left?: T; right?: T; top?: T; bottom?: T }

/** RN-ish style object the reconciler accepts. All lengths are points. */
export interface StyleProps {
  display?: "flex" | "none"
  position?: "relative" | "absolute"
  flexDirection?: "row" | "column" | "row-reverse" | "column-reverse"
  flexWrap?: "nowrap" | "wrap" | "wrap-reverse"
  justifyContent?: keyof typeof KW
  alignItems?: keyof typeof KW
  alignContent?: keyof typeof KW
  alignSelf?: keyof typeof KW
  gap?: number | { width?: LengthPct; height?: LengthPct }
  width?: Dimension
  height?: Dimension
  minWidth?: LengthPctAuto
  minHeight?: LengthPctAuto
  maxWidth?: LengthPctAuto
  maxHeight?: LengthPctAuto
  padding?: Edges<LengthPct>
  margin?: Edges<LengthPctAuto>
  borderWidth?: Edges<LengthPct>
  inset?: Edges<LengthPctAuto>
  left?: LengthPctAuto
  right?: LengthPctAuto
  top?: LengthPctAuto
  bottom?: LengthPctAuto
  flexBasis?: Dimension
  flexGrow?: number
  flexShrink?: number
  aspectRatio?: number
  overflow?: "visible" | "clip" | "hidden" | "scroll" | { x?: string; y?: string }
}

const textEncoder = new TextEncoder()

/** Growable little-endian writer over a pooled Uint8Array. */
class Writer {
  bytes: Uint8Array
  view: DataView
  at = 0
  constructor(bytes?: Uint8Array) {
    this.bytes = bytes ?? new Uint8Array(1 << 16)
    this.view = new DataView(this.bytes.buffer, this.bytes.byteOffset, this.bytes.byteLength)
  }
  reserve(n: number) {
    if (this.at + n <= this.bytes.length) return
    let size = this.bytes.length * 2
    while (size < this.at + n) size *= 2
    const next = new Uint8Array(size)
    next.set(this.bytes.subarray(0, this.at))
    this.bytes = next
    this.view = new DataView(next.buffer, next.byteOffset, next.byteLength)
  }
  u8(v: number) { this.reserve(1); this.bytes[this.at++] = v }
  u16(v: number) { this.reserve(2); this.view.setUint16(this.at, v & 0xffff, true); this.at += 2 }
  u32(v: number) { this.reserve(4); this.view.setUint32(this.at, v >>> 0, true); this.at += 4 }
  u64(v: number | bigint) { this.reserve(8); this.view.setBigUint64(this.at, BigInt(v), true); this.at += 8 }
  f32(v: number) { this.reserve(4); this.view.setFloat32(this.at, v, true); this.at += 4 }
  str(s: string) {
    const bytes = textEncoder.encode(s)
    this.u32(bytes.length)
    this.reserve(bytes.length)
    this.bytes.set(bytes, this.at)
    this.at += bytes.length
  }
}

function pct(s: string): number {
  if (!s.endsWith("%")) throw Error(`expected "<n>%" got "${s}"`)
  return parseFloat(s.slice(0, -1)) / 100
}

function putLP(w: Writer, v: LengthPct) {
  if (typeof v === "number") { w.u8(0); w.f32(v); return }
  w.u8(1); w.f32(pct(v))
}
function putLPA(w: Writer, v: LengthPctAuto) {
  if (v === "auto") { w.u8(2); return }
  putLP(w, v)
}
function putDim(w: Writer, v: Dimension) {
  if (typeof v === "number") { w.u8(0); w.f32(v); return }
  if (v.endsWith("%")) { w.u8(1); w.f32(pct(v)); return }
  switch (v) {
    case "auto": w.u8(2); return
    case "min-content": w.u8(3); return
    case "max-content": w.u8(4); return
    case "fit-content": w.u8(7); return
    case "stretch": w.u8(8); return
    case "content": w.u8(9); return
  }
  throw Error(`bad dimension "${v}"`)
}
function edge4<T>(v: Edges<T> | undefined, zero: T): [T, T, T, T] {
  // [left, right, top, bottom] — taffy Rect order.
  if (v === undefined) return [zero, zero, zero, zero]
  if (v !== null && typeof v === "object") {
    const o = v as { left?: T; right?: T; top?: T; bottom?: T }
    return [o.left ?? zero, o.right ?? zero, o.top ?? zero, o.bottom ?? zero]
  }
  return [v as T, v as T, v as T, v as T]
}

/** Key-order-independent stringify for style interning. */
function canon(v: unknown): string {
  if (v === null || typeof v !== "object") return JSON.stringify(v)!
  if (Array.isArray(v)) return `[${v.map(canon).join(",")}]`
  const o = v as Record<string, unknown>
  return `{${Object.keys(o).sort().map(k => `${JSON.stringify(k)}:${canon(o[k])}`).join(",")}}`
}

const OVERFLOW: Record<string, number> = { visible: 0, clip: 1, hidden: 2, scroll: 3 }

/** Serializes `s`'s present fields positionally under `mask`. */
function putStyle(w: Writer, s: StyleProps) {
  let mask = 0n
  const m = (bit: number) => { mask |= 1n << BigInt(bit) }
  // Two passes would be needed for a tight mask-first encoding; instead we
  // compute the mask from present fields, then write fields in order.
  if (s.display !== undefined) m(0)
  if (s.position !== undefined) m(1)
  if (s.flexDirection !== undefined) m(2)
  if (s.flexWrap !== undefined) m(3)
  if (s.justifyContent !== undefined) m(4)
  if (s.alignItems !== undefined) m(5)
  if (s.alignContent !== undefined) m(6)
  if (s.alignSelf !== undefined) m(7)
  if (s.gap !== undefined) m(8)
  if (s.width !== undefined || s.height !== undefined) m(9)
  if (s.minWidth !== undefined || s.minHeight !== undefined) m(10)
  if (s.maxWidth !== undefined || s.maxHeight !== undefined) m(11)
  if (s.padding !== undefined) m(12)
  if (s.margin !== undefined) m(13)
  if (s.borderWidth !== undefined) m(14)
  const hasInset = s.inset !== undefined || s.left !== undefined || s.right !== undefined
    || s.top !== undefined || s.bottom !== undefined
  if (hasInset) m(15)
  if (s.flexBasis !== undefined) m(16)
  if (s.flexGrow !== undefined) m(17)
  if (s.flexShrink !== undefined) m(18)
  if (s.aspectRatio !== undefined) m(19)
  if (s.overflow !== undefined) m(20)

  w.u64(mask)
  const kw = (v: keyof typeof KW | undefined) => (v === undefined ? UNSET : KW[v]!)

  if (s.display !== undefined) w.u8(s.display === "none" ? 1 : 0)
  if (s.position !== undefined) w.u8(s.position === "absolute" ? 1 : 0)
  if (s.flexDirection !== undefined)
    w.u8({ row: 0, column: 1, "row-reverse": 2, "column-reverse": 3 }[s.flexDirection])
  if (s.flexWrap !== undefined)
    w.u8({ nowrap: 0, wrap: 1, "wrap-reverse": 2 }[s.flexWrap])
  if (s.justifyContent !== undefined) w.u8(kw(s.justifyContent))
  if (s.alignItems !== undefined) w.u8(kw(s.alignItems))
  if (s.alignContent !== undefined) w.u8(kw(s.alignContent))
  if (s.alignSelf !== undefined) w.u8(kw(s.alignSelf))
  if (s.gap !== undefined) {
    const g = s.gap
    if (typeof g === "number") { putLP(w, g); putLP(w, g) }
    else { putLP(w, g.width ?? 0); putLP(w, g.height ?? 0) }
  }
  if (s.width !== undefined || s.height !== undefined) {
    putDim(w, s.width ?? "auto")
    putDim(w, s.height ?? "auto")
  }
  if (s.minWidth !== undefined || s.minHeight !== undefined) {
    putLPA(w, s.minWidth ?? "auto")
    putLPA(w, s.minHeight ?? "auto")
  }
  if (s.maxWidth !== undefined || s.maxHeight !== undefined) {
    putLPA(w, s.maxWidth ?? "auto")
    putLPA(w, s.maxHeight ?? "auto")
  }
  if (s.padding !== undefined) for (const e of edge4(s.padding, 0)) putLP(w, e)
  if (s.margin !== undefined) for (const e of edge4(s.margin, "auto" as const)) putLPA(w, e)
  if (s.borderWidth !== undefined) for (const e of edge4(s.borderWidth, 0)) putLP(w, e)
  if (hasInset) {
    const base = edge4(s.inset, "auto" as const)
    putLPA(w, s.left ?? base[0])
    putLPA(w, s.right ?? base[1])
    putLPA(w, s.top ?? base[2])
    putLPA(w, s.bottom ?? base[3])
  }
  if (s.flexBasis !== undefined) putDim(w, s.flexBasis)
  if (s.flexGrow !== undefined) w.f32(s.flexGrow)
  if (s.flexShrink !== undefined) w.f32(s.flexShrink)
  if (s.aspectRatio !== undefined) w.f32(s.aspectRatio)
  if (s.overflow !== undefined) {
    const o = s.overflow
    if (typeof o === "string") { w.u8(OVERFLOW[o]!); w.u8(OVERFLOW[o]!) }
    else { w.u8(OVERFLOW[o.x ?? "visible"]!); w.u8(OVERFLOW[o.y ?? "visible"]!) }
  }
}

/** One transaction being encoded. Call `finish` to get the wire bytes. */
export class Encoder {
  private w = new Writer()
  private strings: string[] = []
  private stringIx = new Map<string, number>()
  private opBytes = new Writer()
  /** wire style id -> already defined; new styles are interned here. */
  private styleIds = new Map<string, number>()
  private nextStyleId = 0

  private strRef(s: string): number {
    const hit = this.stringIx.get(s)
    if (hit !== undefined) return hit
    const ix = this.strings.length
    this.strings.push(s)
    this.stringIx.set(s, ix)
    return ix
  }

  create(id: number, kind: number) {
    this.opBytes.u8(Op.Create)
    this.opBytes.u32(id)
    this.opBytes.u8(kind)
  }
  setText(id: number, text: string) {
    this.opBytes.u8(Op.SetText)
    this.opBytes.u32(id)
    this.opBytes.u32(this.strRef(text))
  }
  textProps(id: number, fontSize: number, color: number) {
    this.opBytes.u8(Op.TextProps)
    this.opBytes.u32(id)
    this.opBytes.f32(fontSize)
    this.opBytes.u32(color >>> 0)
  }
  setStyle(id: number, wireStyle: number) {
    this.opBytes.u8(Op.SetStyle)
    this.opBytes.u32(id)
    this.opBytes.u32(wireStyle >>> 0)
  }
  place(parent: number, child: number, before: number) {
    this.opBytes.u8(Op.Place)
    this.opBytes.u32(parent)
    this.opBytes.u32(child)
    this.opBytes.u32(before)
  }
  detach(id: number) {
    this.opBytes.u8(Op.Detach)
    this.opBytes.u32(id)
  }
  remove(id: number) {
    this.opBytes.u8(Op.Remove)
    this.opBytes.u32(id)
  }
  hidden(id: number, hidden: boolean) {
    this.opBytes.u8(Op.Hidden)
    this.opBytes.u32(id)
    this.opBytes.u8(hidden ? 1 : 0)
  }
  viewPaint(id: number, color: number) {
    this.opBytes.u8(Op.ViewPaint)
    this.opBytes.u32(id)
    this.opBytes.u32(color >>> 0)
  }

  /** Interns a style; emits a `style` op on first sight and returns its
   * wire id. Identical style objects share one definition. */
  styleIdFor(s: StyleProps | undefined): number {
    if (!s) return NIL
    const key = canon(s)
    const hit = this.styleIds.get(key)
    if (hit !== undefined) return hit
    const id = this.nextStyleId++
    this.styleIds.set(key, id)
    this.opBytes.u8(Op.Style)
    this.opBytes.u32(id)
    putStyle(this.opBytes, s)
    return id
  }

  finish(seq: number | bigint): Uint8Array {
    const w = this.w
    w.u32(MAGIC)
    w.u16(VERSION)
    w.u16(0)
    w.u64(seq)
    w.u32(this.strings.length)
    for (const s of this.strings) w.str(s)
    w.reserve(this.opBytes.at)
    w.bytes.set(this.opBytes.bytes.subarray(0, this.opBytes.at), w.at)
    w.at += this.opBytes.at
    return w.bytes.slice(0, w.at)
  }

  /** Clears per-transaction state (strings + ops) while keeping the
   * buffers and the style table — style definitions persist on the
   * native side for the life of the connection. */
  reset() {
    this.w.at = 0
    this.opBytes.at = 0
    this.strings.length = 0
    this.stringIx.clear()
  }
}
