// CRW2 transaction encoder — the mirror of crates/ui/src/wire.rs.
//
//   header:  magic "CRW2" u32 | version u16 | flags u16 | seq u64
//            string_count u32 | style_count u32 | span_count u32
//   strings: count x (u32 byte_len + utf8)
//   styles:  count x (u64 presence mask + fields in schema order)
//   spans:   count x 28 bytes (start u32, font_size f32, color u32,
//            weight u16, flags u8, reserved u8, family u32, letter
//            spacing f32, line height f32)
//   ops:     u8-tagged records to the end of the buffer
//
// Strings, styles, and spans are per-transaction tables: ops refer to
// them by index and everything resets after `finish`. Nothing persists
// across transactions.

const MAGIC = 0x3257_5243 // "CRW2" little-endian
export const VERSION = 8
export const NIL = 0xffff_ffff // no node / append / default style

const enum Op {
  // structure
  Create = 0x01,
  Place = 0x02,
  Detach = 0x03,
  Remove = 0x04,
  // layout
  Layout = 0x10,
  // spatial
  Spatial = 0x20,
  Layer = 0x22,
  // paint
  Paint = 0x30,
  // text
  Paragraph = 0x40,
  InputConfig = 0x41,
  // semantics
  Role = 0x50,
  Label = 0x51,
  // interaction
  Interaction = 0x60,
  Claims = 0x61,
  Trap = 0x62,
  // payload
  Surface = 0x70,
  Payload = 0x71,
  Drawing = 0x72,
  ImageConfig = 0x73,
  // command
  Command = 0x80,
  // lists
  ListConfig = 0x90,
  ListSplice = 0x91,
  ListIndex = 0x92,
  ScrollAnchor = 0x93,
  // animation
  Transition = 0xa0,
  Animate = 0xa1,
  // state styles
  States = 0xb0,
  Variants = 0xb1,
  Environment = 0xb2,
  Color = 0xb3,
}

/** `WireShape.current` bits (craie-vector's `svg::CURRENT_FILL` and
 * `CURRENT_STROKE`). */
export const CURRENT = { fill: 1, stroke: 2 } as const

/** One shape of a runtime vector drawing: SVG strings plus resolved
 * paint. Mirrors craie-vector's `svg::Shape`; colors are RGBA u32 (alpha
 * 0 is `none`), empty strings are absent attributes. */
export interface WireShape {
  /** 0 path (`d`), 1 polyline, 2 polygon (`points`). */
  kind: number
  geometry: string
  transform: string
  /** `stroke-dasharray` as lengths ("4 2"), or "" for solid. */
  dashes: string
  fill: number
  /** 0 nonzero, 1 evenodd. */
  fillRule: number
  stroke: number
  /** `CURRENT` bits: the fill or the stroke paints with the node's
   * inherited color (`currentColor`), and its color is the tint (white
   * with the paint's opacity as alpha). */
  current: number
  strokeWidth: number
  /** 0 miter, 1 round, 2 bevel. */
  join: number
  /** 0 butt, 1 round, 2 square. */
  cap: number
  miterLimit: number
  dashOffset: number
  opacity: number
}

/** Animatable properties — mirror animation.rs `Prop`. */
export const ANIM_PROP = {
  transform: 0,
  opacity: 1,
  backgroundColor: 2,
  borderColor: 3,
  width: 4,
  height: 5,
  padding: 6,
  gap: 7,
  /** The inherited color (text, inputs, `currentColor` drawings):
   * tweens between two set colors. */
  color: 8,
} as const
export type AnimProp = keyof typeof ANIM_PROP

/** CSS named easings as cubic-bezier control points. */
export const EASING = {
  linear: [0, 0, 1, 1],
  ease: [0.25, 0.1, 0.25, 1],
  "ease-in": [0.42, 0, 1, 1],
  "ease-out": [0, 0, 0.58, 1],
  "ease-in-out": [0.42, 0, 0.58, 1],
} as const
export type Easing = keyof typeof EASING | readonly [number, number, number, number]

/** A timing in milliseconds (as RN Animated): a CSS curve over
 * `duration` (default easing `ease`), or a spring (defaults: stiffness
 * 170, damping 26, mass 1). */
export type Timing =
  | { duration: number; delay?: number; easing?: Easing }
  | { spring: { stiffness?: number; damping?: number; mass?: number }; delay?: number }

/** `style.transition`: later changes of these properties tween. */
export type Transitions = Partial<Record<AnimProp, Timing>>

/** Scroll anchoring policies — mirror mutation.rs `Anchor`. */
export const ANCHOR = { "keep-visible": 0, "stick-to-end": 1, none: 2 } as const
export type ScrollAnchor = keyof typeof ANCHOR

/** A row template for native estimates — mirror mutation.rs
 * `ItemTemplate`: fixed extent, horizontal insets, wrapping font size. */
export interface ListTemplate {
  base?: number
  inset?: number
  fontSize?: number
}
/** An item's description for native estimates: its template and text
 * length in characters. `id` is the item's identity (the bridge interns
 * the item's key); NIL for none. */
export interface ItemDesc {
  template?: number
  textLength?: number
  id?: number
  /** The same (unchanged) item the splice removes under `id`: a move. */
  unchanged?: boolean
}

// Field mask bits — mirror wire.rs `spatial_field` / `paint_field`.
const SPATIAL_FIELD = { TRANSFORM: 1 << 0, OPACITY: 1 << 1, Z: 1 << 2 } as const
const PAINT_FIELD = { FILL: 1 << 0, RADIUS: 1 << 1, BORDER: 1 << 2 } as const
const SPAN_ITALIC = 1 << 0
const SPAN_UNDERLINE = 1 << 1
const SPAN_LINE_THROUGH = 1 << 2
const SPAN_INHERIT_COLOR = 1 << 3
const SPAN_PRESSABLE = 1 << 4
const SPAN_PRESS_JOINS = 1 << 5
/** Span decoration bits (`TextSpanIn.decoration`). */
export const DECORATION = { underline: 1, lineThrough: 2 } as const

// COMMAND op sub-tags — mirror wire.rs `mod cmd`.
const enum Cmd {
  Focus = 0,
  Blur = 1,
  SetText = 2,
  ScrollTo = 3,
  InsertText = 4,
  WriteClipboard = 5,
}

/** Accessibility roles — mirror mutation.rs `Role`. */
export const ROLE = {
  none: 0,
  button: 1,
  text: 2,
  textInput: 3,
  multilineTextInput: 4,
  scrollView: 5,
  image: 6,
  header: 7,
  link: 8,
  checkbox: 9,
  adjustable: 10,
  list: 11,
  listItem: 12,
  group: 13,
  switch: 14,
  radio: 15,
  radiogroup: 16,
} as const
export type AccessibilityRole = keyof typeof ROLE

/** States a node reports while false, set when the prop was given —
 * mirror mutation.rs `reported`. */
export const REPORTED = { expanded: 1, selected: 2 } as const

/** Built-in surface kinds — mirror surface.rs `kind`. */
export const SURFACE = { bars: 1 } as const

// Outbound event kinds + listener mask bits — mirror events.rs.
export const EVENT_KIND = {
  pointerMove: 1,
  pointerDown: 2,
  pointerUp: 3,
  pointerEnter: 4,
  pointerLeave: 5,
  wheel: 6,
  keyDown: 7,
  keyUp: 8,
  focus: 9,
  blur: 10,
  change: 11,
  submit: 12,
  scroll: 13,
  /** A list's rendered range: a = first, b = end, x = kept item index
   * (-1), y = the list's splice revision (mod 2^24), key = kept item id. */
  listRange: 14,
  /** An `animate` tween ended: key = property | reason << 8. */
  animationEnd: 15,
  /** Native frame statistics (node NIL): x = frames per second, y = mean
   * CPU ms per frame, a = the largest, b = mean layout and scene ms,
   * key = live nodes, revision = running tweens. */
  frameStats: 16,
  /** A claim matched (claims.rs): node = the claimer (NIL: the window
   * list), key = claim kind | claim index << 8, revision = the claim
   * set's version, text = the payload (clipboard or selected text,
   * dropped paths joined by "\\0"). */
  claim: 17,
  /** An image node decoded (key 0: x/y = the natural size in pixels) or
   * failed (key 1: text = why). */
  image: 18,
  /** A pressable's press (press.rs; primary button only): key = mods |
   * phase << 4 (`PRESS_PHASE`) | button << 8 | span + 1 << 16 (0: none),
   * x/y = the pointer, a/b = node-relative, revision = the paragraph
   * revision when a span is set. */
  press: 19,
  /** A pressable activated, once per pointer click, Enter or Space, or
   * accessibility click: key = mods | source << 4 (`ACTIVATE_SOURCE`) |
   * button << 8 (1: primary, 0: none) | span + 1 << 16, x/y = the
   * release point or else the node's center, a/b = node-relative. */
  activate: 20,
} as const

/** A press event's phase — mirror events.rs `press_phase`. */
export const PRESS_PHASE = { in: 0, out: 1, cancel: 2 } as const
/** Where an activation came from — mirror events.rs `activate_source`. */
export const ACTIVATE_SOURCE = ["pointer", "keyboard", "accessibility"] as const
export type ActivateSource = (typeof ACTIVATE_SOURCE)[number]

/** Press flags on a node's interaction — mirror mutation.rs `press`:
 * the node owns presses and activates; a disabled one swallows them;
 * pressing it keeps focus where it is. */
export const PRESS_FLAG = { pressable: 1, disabled: 2, keepFocus: 4 } as const

/** How an image fills its box — mirror image.rs `Fit`. */
export const FIT = { cover: 0, contain: 1, fill: 2 } as const
export type ImageFit = keyof typeof FIT

/** Claim kinds and chord flags — mirror claims.rs. */
export const CLAIM_KIND = { key: 1, paste: 2, copy: 3, cut: 4, drop: 5, contextMenu: 6 } as const
export const CHORD_FLAG = { named: 1, noRepeat: 2, inInput: 4 } as const
/** Interaction op flag bits — mirror mutation.rs `interaction_flag`.
 * `inert`: no hit testing, focus or accessibility for the node and its
 * subtree. `autoFocus`: the node a focus trap focuses on activation.
 * Bits 4 to 6 are the `PRESS_FLAG` bits, shifted by `pressShift`. */
export const INTERACTION = { focusable: 1, selectable: 2, inert: 4, autoFocus: 8, pressShift: 4 } as const
/** Trap op flag bits — mirror mutation.rs `trap_flag`. */
export const TRAP = { active: 1, modal: 2, autoFocus: 4, restoreFocus: 8 } as const
/** Modifier bits (key records, pointer records, chords) — mirror
 * events.rs `Mods`. */
export const MODS = { shift: 1, ctrl: 2, alt: 4, meta: 8 } as const

/** Named keys by the web's `event.key`, lower case, plus `space` —
 * mirror events.rs `Key::code`. A key record's named key sits in key
 * bits 8 to 15. */
export const KEY_CODE: Readonly<Record<string, number>> = (() => {
  const named: Record<string, number> = {
    backspace: 1, tab: 2, enter: 3, escape: 4,
    arrowleft: 5, arrowup: 6, arrowright: 7, arrowdown: 8,
    home: 9, end: 10, pageup: 11, pagedown: 12, delete: 13,
    " ": 14, space: 14, insert: 15, contextmenu: 16,
  }
  for (let f = 1; f <= 24; f++) named[`f${f}`] = 31 + f
  return named
})()

/** When an input's Enter submits — mirror input.rs `SubmitKey`. */
export const SUBMIT_KEY = { enter: 0, "mod+enter": 1, none: 2 } as const
export type SubmitKey = keyof typeof SUBMIT_KEY

/** One claim on the wire (claims.rs `Claim`). */
export interface Claim {
  kind: number
  flags: number
  mods: number
  /** A named key's code (`CHORD_FLAG.named`), else a character's code
   * point. */
  key: number
}

/** A chord such as `mod+shift+o`, `escape`, `shift+?` or `alt+arrowup`
 * as a key claim, or `null` when it names no key. Modifiers must match
 * exactly; `mod` is Cmd on Apple platforms and Ctrl elsewhere (`apple`).
 * The key is a named key (`KEY_CODE`) or one character as the web's
 * `event.key` gives it, lower-cased: Shift+/ is `shift+?`, and
 * `shift+1` never matches on a US layout, where Shift+1 gives "!" (as
 * in Marbre's `matchesChord`). Native matches letters and digits held
 * with Alt, and letters of non-Latin layouts, by physical key
 * (claims.rs). */
export function parseChord(chord: string, apple: boolean): Claim | null {
  const lower = chord.toLowerCase()
  // `+` and `mod++` are the plus key.
  const plus = lower === "+" || lower.endsWith("++")
  const cut = plus ? lower.length - 1 : lower.lastIndexOf("+") + 1
  const key = lower.slice(cut)
  let mods = 0
  if (cut > 0) {
    for (const m of lower.slice(0, cut - 1).split("+")) {
      if (m === "mod") mods |= apple ? MODS.meta : MODS.ctrl
      else if (Object.hasOwn(MODS, m)) mods |= MODS[m as keyof typeof MODS]
      else return null
    }
  }
  const named = Object.hasOwn(KEY_CODE, key) ? KEY_CODE[key] : undefined
  if (named !== undefined) return { kind: CLAIM_KIND.key, flags: CHORD_FLAG.named, mods, key: named }
  const cp = key.codePointAt(0)
  if (cp === undefined || String.fromCodePoint(cp) !== key) return null
  return { kind: CLAIM_KIND.key, flags: 0, mods, key: cp }
}

/** Why a tween ended — mirror animation.rs `end_reason`. */
export const END_REASON = ["finished", "cancelled", "retargeted", "removed"] as const
export type EndReason = (typeof END_REASON)[number]

/** What `node.animate` resolves with: `finished` when it reached its
 * target. */
export interface AnimationEnd {
  finished: boolean
  reason: EndReason
}

export const EVENT_MASK = {
  pointerMove: 1 << 0,
  pointerDown: 1 << 1,
  pointerUp: 1 << 2,
  pointerEnterLeave: 1 << 3,
  wheel: 1 << 4,
  key: 1 << 5,
  focus: 1 << 6,
  input: 1 << 7,
  scroll: 1 << 8,
  press: 1 << 9,
  activate: 1 << 10,
} as const

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
  /** Spatial, not layout: travels in its own op and never relayouts.
   * RN-style list, applied about the border-box center. */
  transform?: Transform
  /** Spatial: group opacity in [0, 1]. */
  opacity?: number
  /** Spatial: the order among siblings, an integer (React Native's
   * `zIndex`: no stacking contexts, ties keep tree order). Never
   * relayouts; Tab order and accessibility keep tree order. */
  zIndex?: number
  /** Not layout: declared transitions, sent in their own op. */
  transition?: Transitions
}

/** One RN-style transform step. Angles: "45deg", "0.5rad", or radians. */
export type TransformStep =
  | { translateX: number }
  | { translateY: number }
  | { scale: number }
  | { scaleX: number }
  | { scaleY: number }
  | { rotate: string | number }
  | { skewX: string | number }
  | { skewY: string | number }
  | { matrix: readonly [number, number, number, number, number, number] }
export type Transform = readonly TransformStep[]

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
  // An undefined field is an absent one (`putStyle` skips both).
  const o = v as Record<string, unknown>
  return `{${Object.keys(o).filter(k => o[k] !== undefined).sort().map(k => `${JSON.stringify(k)}:${canon(o[k])}`).join(",")}}`
}

const OVERFLOW: Record<string, number> = { visible: 0, clip: 1, hidden: 2, scroll: 3 }

/** The style fields `s` has (`states.rs` `wire::field` bits). */
function styleMask(s: StyleProps): bigint {
  let mask = 0n
  const m = (bit: number) => { mask |= 1n << BigInt(bit) }
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
  if (s.inset !== undefined || s.left !== undefined || s.right !== undefined
    || s.top !== undefined || s.bottom !== undefined) m(15)
  if (s.flexBasis !== undefined) m(16)
  if (s.flexGrow !== undefined) m(17)
  if (s.flexShrink !== undefined) m(18)
  if (s.aspectRatio !== undefined) m(19)
  if (s.overflow !== undefined) m(20)
  return mask
}

/** Serializes `s`'s present fields: the mask, then the fields. */
function putStyle(w: Writer, s: StyleProps) {
  const mask = styleMask(s)
  w.u64(mask)
  putStyleFields(w, s, mask)
}

/** Serializes the fields in `mask` positionally; absent parts of a
 * field (the height of a SIZE) take their defaults. */
function putStyleFields(w: Writer, s: StyleProps, mask: bigint) {
  const has = (bit: number) => (mask & (1n << BigInt(bit))) !== 0n
  const kw = (v: keyof typeof KW | undefined) => (v === undefined ? UNSET : KW[v]!)

  if (has(0)) w.u8(s.display === "none" ? 1 : 0)
  if (has(1)) w.u8(s.position === "absolute" ? 1 : 0)
  if (has(2))
    w.u8({ row: 0, column: 1, "row-reverse": 2, "column-reverse": 3 }[s.flexDirection ?? "row"])
  if (has(3))
    w.u8({ nowrap: 0, wrap: 1, "wrap-reverse": 2 }[s.flexWrap ?? "nowrap"])
  if (has(4)) w.u8(kw(s.justifyContent))
  if (has(5)) w.u8(kw(s.alignItems))
  if (has(6)) w.u8(kw(s.alignContent))
  if (has(7)) w.u8(kw(s.alignSelf))
  if (has(8)) {
    const g = s.gap ?? 0
    if (typeof g === "number") { putLP(w, g); putLP(w, g) }
    else { putLP(w, g.width ?? 0); putLP(w, g.height ?? 0) }
  }
  if (has(9)) {
    putDim(w, s.width ?? "auto")
    putDim(w, s.height ?? "auto")
  }
  if (has(10)) {
    putLPA(w, s.minWidth ?? "auto")
    putLPA(w, s.minHeight ?? "auto")
  }
  if (has(11)) {
    putLPA(w, s.maxWidth ?? "auto")
    putLPA(w, s.maxHeight ?? "auto")
  }
  if (has(12)) for (const e of edge4(s.padding, 0)) putLP(w, e)
  // Unset margin sides are 0 (CSS and React Native), not auto.
  if (has(13)) for (const e of edge4<LengthPctAuto>(s.margin, 0)) putLPA(w, e)
  if (has(14)) for (const e of edge4(s.borderWidth, 0)) putLP(w, e)
  if (has(15)) {
    const base = edge4(s.inset, "auto" as const)
    putLPA(w, s.left ?? base[0])
    putLPA(w, s.right ?? base[1])
    putLPA(w, s.top ?? base[2])
    putLPA(w, s.bottom ?? base[3])
  }
  if (has(16)) putDim(w, s.flexBasis ?? "auto")
  if (has(17)) w.f32(s.flexGrow ?? 0)
  if (has(18)) w.f32(s.flexShrink ?? 1)
  if (has(19)) w.f32(s.aspectRatio ?? NaN)
  if (has(20)) {
    const o = s.overflow ?? "visible"
    if (typeof o === "string") { w.u8(OVERFLOW[o]!); w.u8(OVERFLOW[o]!) }
    else { w.u8(OVERFLOW[o.x ?? "visible"]!); w.u8(OVERFLOW[o.y ?? "visible"]!) }
  }
}

/** One style span of a paragraph (span zero starts at 0). */
export interface TextSpanIn {
  start: number
  fontSize: number
  color: number
  weight?: number
  italic?: boolean
  /** Family name or generic ("monospace"); absent: the default family. */
  fontFamily?: string
  /** `DECORATION` bits. */
  decoration?: number
  /** Added to each character's advance, logical points. */
  letterSpacing?: number
  /** Absolute line height, logical points; span zero's applies to the
   * paragraph. */
  lineHeight?: number
  /** Draw in the nearest inherited color (`Encoder.color` on the text
   * or an ancestor); `color` when there is none. */
  inheritColor?: boolean
  /** A nested Text with `onPress`: presses on the span go to its text
   * node, with the span's index. */
  pressable?: boolean
  /** A pressable span of the same pressable Text as the span before it
   * (`<Text onPress>See <Text weight={700}>logs</Text></Text>`): a
   * press on one and a release on the other activate. */
  pressJoins?: boolean
}

/** The layout keys `s` sets — mirror states.rs `layout_key`: one per
 * property, axis and side (left, right, top, bottom), so two variants
 * setting different sides of `padding` both apply: `padding: { left: 4 }`
 * is one key, `padding: 4` four. */
export function layoutKeys(s: StyleProps): bigint {
  let keys = 0n
  const k = (bit: number) => { keys |= 1n << BigInt(bit) }
  // Four sides from bit `at`: a single value sets all four.
  const sides = (v: unknown, at: number) => {
    if (v === undefined) return
    if (v === null || typeof v !== "object") { for (let i = 0; i < 4; i++) k(at + i); return }
    const o = v as Record<string, unknown>
    ;["left", "right", "top", "bottom"].forEach((side, i) => { if (o[side] !== undefined) k(at + i) })
  }
  const axes = (v: unknown, x: string, y: string, at: number) => {
    if (v === undefined) return
    if (v === null || typeof v !== "object") { k(at); k(at + 1); return }
    const o = v as Record<string, unknown>
    if (o[x] !== undefined) k(at)
    if (o[y] !== undefined) k(at + 1)
  }
  const one = [s.display, s.position, s.flexDirection, s.flexWrap, s.justifyContent,
    s.alignItems, s.alignContent, s.alignSelf]
  one.forEach((v, i) => { if (v !== undefined) k(i) })
  axes(s.gap, "width", "height", 8)
  if (s.width !== undefined) k(10)
  if (s.height !== undefined) k(11)
  if (s.minWidth !== undefined) k(12)
  if (s.minHeight !== undefined) k(13)
  if (s.maxWidth !== undefined) k(14)
  if (s.maxHeight !== undefined) k(15)
  sides(s.padding, 16)
  sides(s.margin, 20)
  sides(s.borderWidth, 24)
  sides(s.inset, 28)
  ;[s.left, s.right, s.top, s.bottom].forEach((v, i) => { if (v !== undefined) k(28 + i) })
  if (s.flexBasis !== undefined) k(32)
  if (s.flexGrow !== undefined) k(33)
  if (s.flexShrink !== undefined) k(34)
  if (s.aspectRatio !== undefined) k(35)
  axes(s.overflow, "x", "y", 36)
  return keys
}

/** The style fields that carry `keys` (states.rs `layout_key::fields`). */
function keyFields(keys: bigint): bigint {
  const any = (bits: bigint, field: number) => ((keys & bits) !== 0n ? 1n << BigInt(field) : 0n)
  return (keys & 0xffn)
    | any(0b11n << 8n, 8) | any(1n << 10n | 1n << 11n, 9) | any(0b11n << 12n, 10)
    | any(0b11n << 14n, 11) | any(0xfn << 16n, 12) | any(0xfn << 20n, 13)
    | any(0xfn << 24n, 14) | any(0xfn << 28n, 15) | any(1n << 32n, 16)
    | any(1n << 33n, 17) | any(1n << 34n, 18) | any(1n << 35n, 19) | any(0b11n << 36n, 20)
}

/** State bit indices — mirror states.rs `state_bit`. Bits below
 * `CUSTOM_STATES` are custom states, in declaration order. A higher bit
 * outranks a lower one at equal depth. */
export const STATE_BIT = {
  hover: 54,
  focusWithin: 55,
  focusVisible: 56,
  focusVisibleWithin: 57,
  expanded: 58,
  selected: 59,
  checked: 60,
  highlighted: 61,
  pressed: 62,
  disabled: 63,
} as const
export type StateName = keyof typeof STATE_BIT
export const CUSTOM_STATES = 54
/** Environment bits — mirror states.rs `env_bit`. */
export const ENV_BIT = { narrow: 1, compact: 2, touch: 4, reducedMotion: 8 } as const
export type EnvName = keyof typeof ENV_BIT

/** A variant's values; absent ones are not overridden. */
export interface VariantValues {
  fill?: number
  borderColor?: number
  borderWidth?: number
  radius?: number
  /** The inherited color (text, inputs, `currentColor` drawings);
   * `null` clears it. */
  color?: number | null
  opacity?: number
  transform?: Affine
  /** Layout values: each key it sets applies on its own (`height`
   * alone keeps the width that applies). */
  layout?: StyleProps
}

/** A variant: `values` apply while every term's scope holds all bits of
 * its `mask` and the environment has every bit of `env`. */
export interface VariantIn {
  terms: readonly { scope: number; mask: bigint }[]
  env: number
  values: VariantValues
}

// Variant value bits — mirror states.rs `value_field`.
const VALUE_FIELD = {
  FILL: 1 << 0, BORDER_COLOR: 1 << 1, RADIUS: 1 << 2, COLOR: 1 << 3,
  OPACITY: 1 << 4, TRANSFORM: 1 << 5, LAYOUT: 1 << 6, BORDER_WIDTH: 1 << 7,
} as const

export type Affine = [number, number, number, number, number, number]
export const IDENTITY: Affine = [1, 0, 0, 1, 0, 0]

function angle(v: string | number): number {
  if (typeof v === "number") return v
  if (v.endsWith("deg")) return (parseFloat(v) * Math.PI) / 180
  if (v.endsWith("rad")) return parseFloat(v)
  throw Error(`bad angle "${v}"`)
}

/** p · c: apply `c` first, then `p` (CSS matrix order, like Affine::mul). */
function mul(p: Affine, c: Affine): Affine {
  const [a, b, cc, d, e, f] = p
  const [a2, b2, c2, d2, e2, f2] = c
  return [
    a * a2 + cc * b2,
    b * a2 + d * b2,
    a * c2 + cc * d2,
    b * c2 + d * d2,
    a * e2 + cc * f2 + e,
    b * e2 + d * f2 + f,
  ]
}

/** A timing on the wire: kind u8, delay f32, five f32 (a curve:
 * duration, x1, y1, x2, y2; a spring: stiffness, damping, mass, 0, 0),
 * seconds natively. Checks what native checks (finite, delays and
 * durations up to 600 s, curve x in [0, 1], positive springs) and
 * throws. */
export function timingValues(t: Timing): number[] {
  const secs = (ms: unknown, what: string) => {
    if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0 || ms > 600_000) {
      throw Error(`bad ${what} ${String(ms)}`)
    }
    return ms / 1000
  }
  const delay = secs(t.delay ?? 0, "delay")
  if ("spring" in t) {
    const s = [t.spring.stiffness ?? 170, t.spring.damping ?? 26, t.spring.mass ?? 1]
    if (!s.every(v => typeof v === "number" && Number.isFinite(v) && v > 0)) {
      throw Error("spring stiffness, damping, and mass must be positive")
    }
    return [1, delay, ...s, 0, 0]
  }
  const e = typeof t.easing === "object" ? t.easing : EASING[t.easing ?? "ease"]
  if (!e || e.length !== 4 || !e.every(v => typeof v === "number" && Number.isFinite(v))) {
    throw Error(`unknown easing "${String(t.easing)}"`)
  }
  if (!(e[0]! >= 0 && e[0]! <= 1 && e[2]! >= 0 && e[2]! <= 1)) throw Error("easing x outside [0, 1]")
  return [0, delay, secs(t.duration, "duration"), ...e]
}

/** Writes values from `timingValues`. */
function putTiming(b: Writer, v: readonly number[]) {
  b.u8(v[0]!)
  for (let i = 1; i < 7; i++) b.f32(v[i]!)
}

/** Folds an RN-style transform list into one matrix. Like CSS, the list
 * composes left to right, so the last step applies to points first. */
export function transformMatrix(t: Transform | undefined): Affine {
  let m: Affine = IDENTITY
  for (const step of t ?? []) {
    const [k, v] = Object.entries(step)[0] as [string, any]
    let s: Affine
    switch (k) {
      case "translateX": s = [1, 0, 0, 1, v, 0]; break
      case "translateY": s = [1, 0, 0, 1, 0, v]; break
      case "scale": s = [v, 0, 0, v, 0, 0]; break
      case "scaleX": s = [v, 0, 0, 1, 0, 0]; break
      case "scaleY": s = [1, 0, 0, v, 0, 0]; break
      case "rotate": {
        const r = angle(v)
        s = [Math.cos(r), Math.sin(r), -Math.sin(r), Math.cos(r), 0, 0]
        break
      }
      case "skewX": s = [1, 0, Math.tan(angle(v)), 1, 0, 0]; break
      case "skewY": s = [1, Math.tan(angle(v)), 0, 1, 0, 0]; break
      case "matrix": s = [...v] as Affine; break
      default: throw Error(`unknown transform "${k}"`)
    }
    m = mul(m, s)
  }
  return m
}

/** The layout part of a style: everything but the spatial keys, or
 * undefined when nothing is left (`{ zIndex: 1 }` has no layout, like
 * no style at all). */
export function layoutPart(s: StyleProps | undefined): StyleProps | undefined {
  if (!s) return undefined
  let rest = s
  if (
    s.transform !== undefined || s.opacity !== undefined || s.zIndex !== undefined ||
    s.transition !== undefined
  ) {
    const { transform: _t, opacity: _o, zIndex: _z, transition: _tr, ...layout } = s
    rest = layout
  }
  for (const k in rest) if (rest[k as keyof StyleProps] !== undefined) return rest
  return undefined
}

/** Key-order-independent stringify for style interning. */
export function styleKey(s: StyleProps | undefined): string {
  return s ? canon(s) : ""
}

/** One transaction being encoded. Call `finish` to get the wire bytes;
 * every table resets afterwards. */
export class Encoder {
  private out = new Writer()
  private ops = new Writer()
  private styleBytes = new Writer()
  private spanBytes = new Writer()
  private strings: string[] = []
  private stringIx = new Map<string, number>()
  private styleIx = new Map<string, number>()
  private styleCount = 0
  private spanCount = 0
  /** Span lists interned by content: most paragraphs share one style. */
  private spanIx = new Map<string, number>()

  private strRef(s: string): number {
    const hit = this.stringIx.get(s)
    if (hit !== undefined) return hit
    const ix = this.strings.length
    this.strings.push(s)
    this.stringIx.set(s, ix)
    return ix
  }

  /** Interns a layout style into this transaction's table. */
  private styleRef(s: StyleProps | undefined): number {
    if (!s) return NIL
    const key = canon(s)
    const hit = this.styleIx.get(key)
    if (hit !== undefined) return hit
    const ix = this.styleCount++
    this.styleIx.set(key, ix)
    putStyle(this.styleBytes, s)
    return ix
  }

  /** Whether any op has been recorded since the last `finish`. */
  get empty(): boolean {
    return this.ops.at === 0
  }

  create(id: number, kind: number) {
    this.ops.u8(Op.Create)
    this.ops.u32(id)
    this.ops.u8(kind)
  }
  place(parent: number, child: number, before: number) {
    this.ops.u8(Op.Place)
    this.ops.u32(parent)
    this.ops.u32(child)
    this.ops.u32(before)
  }
  detach(id: number) {
    this.ops.u8(Op.Detach)
    this.ops.u32(id)
  }
  remove(id: number) {
    this.ops.u8(Op.Remove)
    this.ops.u32(id)
  }
  /** Sets a node's layout inputs; `undefined` restores the defaults.
   * Spatial keys must already be split off (see `layoutPart`). */
  layout(id: number, style: StyleProps | undefined) {
    const ref = this.styleRef(style)
    this.ops.u8(Op.Layout)
    this.ops.u32(id)
    this.ops.u32(ref)
  }
  /** Transform (CSS matrix about the center), opacity and/or z (an
   * i32: the order among siblings). */
  spatial(id: number, transform?: Affine, opacity?: number, z?: number) {
    const b = this.ops
    b.u8(Op.Spatial)
    b.u32(id)
    b.u8(
      (transform !== undefined ? SPATIAL_FIELD.TRANSFORM : 0) |
        (opacity !== undefined ? SPATIAL_FIELD.OPACITY : 0) |
        (z !== undefined ? SPATIAL_FIELD.Z : 0),
    )
    if (transform !== undefined) for (const v of transform) b.f32(v)
    if (opacity !== undefined) b.f32(opacity)
    if (z !== undefined) b.u32(z >>> 0)
  }
  /** Makes `id` a layer container: hit testing passes through its own
   * box, and it never sorts below the sibling holding `owner` (NIL:
   * none). */
  layer(id: number, owner: number) {
    this.ops.u8(Op.Layer)
    this.ops.u32(id)
    this.ops.u32(owner)
  }
  /** Masked paint update: fill, corner radius, border (color, width). */
  paint(
    id: number,
    fill?: number,
    radius?: number,
    border?: { color: number; width: number },
  ) {
    const b = this.ops
    b.u8(Op.Paint)
    b.u32(id)
    b.u8(
      (fill !== undefined ? PAINT_FIELD.FILL : 0) |
        (radius !== undefined ? PAINT_FIELD.RADIUS : 0) |
        (border !== undefined ? PAINT_FIELD.BORDER : 0),
    )
    if (fill !== undefined) b.u32(fill >>> 0)
    if (radius !== undefined) b.f32(radius)
    if (border !== undefined) { b.u32(border.color >>> 0); b.f32(border.width) }
  }
  /** A paragraph: UTF-8 text plus its style span list. Span starts are
   * UTF-8 byte offsets; span zero starts at 0. */
  paragraph(id: number, text: string, spans: readonly TextSpanIn[]) {
    const s = this.strRef(text)
    // JSON: family names are escaped and the list keeps its row
    // boundaries, so different span lists never share a key.
    const key = JSON.stringify(spans.map(sp => [
      sp.start, sp.fontSize, sp.color >>> 0, sp.weight ?? 400, sp.italic ? 1 : 0,
      sp.decoration ?? 0, sp.letterSpacing ?? 0, sp.lineHeight ?? 0, sp.fontFamily || null,
      sp.inheritColor ? 1 : 0, sp.pressable ? 1 : 0, sp.pressJoins ? 1 : 0,
    ]))
    let start = this.spanIx.get(key)
    if (start === undefined) {
      start = this.spanCount
      this.spanIx.set(key, start)
      for (const sp of spans) {
        const w = this.spanBytes
        w.u32(sp.start)
        w.f32(sp.fontSize)
        w.u32(sp.color >>> 0)
        w.u16(sp.weight ?? 400)
        const d = sp.decoration ?? 0
        w.u8(
          (sp.italic ? SPAN_ITALIC : 0) |
          (d & DECORATION.underline ? SPAN_UNDERLINE : 0) |
          (d & DECORATION.lineThrough ? SPAN_LINE_THROUGH : 0) |
          (sp.inheritColor ? SPAN_INHERIT_COLOR : 0) |
          (sp.pressable ? SPAN_PRESSABLE : 0) |
          (sp.pressJoins ? SPAN_PRESS_JOINS : 0),
        )
        w.u8(0)
        w.u32(sp.fontFamily ? this.strRef(sp.fontFamily) : NIL)
        w.f32(sp.letterSpacing ?? 0)
        w.f32(sp.lineHeight ?? 0)
        this.spanCount++
      }
    }
    this.ops.u8(Op.Paragraph)
    this.ops.u32(id)
    this.ops.u32(s)
    this.ops.u32(start)
    this.ops.u32(spans.length)
  }
  /** `submit`: a `SUBMIT_KEY` value. */
  inputConfig(
    id: number,
    fontSize: number,
    placeholder: string,
    multiline: boolean,
    submit: number,
  ) {
    const s = this.strRef(placeholder)
    this.ops.u8(Op.InputConfig)
    this.ops.u32(id)
    this.ops.f32(fontSize)
    this.ops.u32(s)
    this.ops.u8((multiline ? 1 : 0) | submit << 1)
  }
  /** The role, and the states it reports while false (`REPORTED`). */
  role(id: number, role: number, reported = 0) {
    this.ops.u8(Op.Role)
    this.ops.u32(id)
    this.ops.u8(role)
    this.ops.u8(reported)
  }
  /** Accessibility name (empty string clears it). */
  label(id: number, text: string) {
    const s = this.strRef(text)
    this.ops.u8(Op.Label)
    this.ops.u32(id)
    this.ops.u32(s)
  }
  /** Listener mask and `INTERACTION` flags: `selectable` makes the
   * node's text descendants one selection domain; `inert` takes its
   * subtree out of hit testing, focus and accessibility; `autoFocus`
   * marks a trap's first focus; the press flags sit in bits 4 to 6. */
  interaction(id: number, listeners: number, flags: number) {
    this.ops.u8(Op.Interaction)
    this.ops.u32(id)
    this.ops.u32(listeners >>> 0)
    this.ops.u8(flags)
  }
  /** A focus trap on `id` (`TRAP` flags). While active, Tab cycles in
   * its scope (its subtree and the layers it owns); modal, everything
   * else is inert. Clearing `active` deactivates it, as removing the
   * node does. */
  trap(id: number, flags: number) {
    this.ops.u8(Op.Trap)
    this.ops.u32(id)
    this.ops.u8(flags)
  }
  /** A node's claim set (id NIL: the window list), replacing the one
   * before; an empty set removes it. */
  claims(id: number, version: number, claims: readonly Claim[]) {
    const b = this.ops
    b.u8(Op.Claims)
    b.u32(id)
    b.u32(version >>> 0)
    b.u16(claims.length)
    for (const c of claims) {
      b.u8(c.kind)
      b.u8(c.flags)
      b.u8(c.mods)
      b.u8(0)
      b.u32(c.key)
    }
  }
  surface(id: number, kind: number, params: readonly number[]) {
    this.ops.u8(Op.Surface)
    this.ops.u32(id)
    this.ops.u32(kind >>> 0)
    for (let i = 0; i < 4; i++) this.ops.u32((params[i] ?? 0) >>> 0)
  }
  /** Surface payload: the typed array's bytes, copied once. */
  payload(id: number, bytes: ArrayBufferView) {
    const view = new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength)
    const b = this.ops
    b.u8(Op.Payload)
    b.u32(id)
    b.u32(view.byteLength)
    b.reserve(view.byteLength)
    b.bytes.set(view, b.at)
    b.at += view.byteLength
  }
  /** Runtime vector drawing: a view box and shapes, 45 bytes each. */
  drawing(id: number, viewBox: string, shapes: readonly WireShape[]) {
    const view = this.strRef(viewBox)
    const b = this.ops
    b.u8(Op.Drawing)
    b.u32(id)
    b.u32(view)
    b.u16(shapes.length)
    for (const s of shapes) {
      const refs = [this.strRef(s.geometry), this.strRef(s.transform), this.strRef(s.dashes)]
      b.u8(s.kind)
      b.u8(s.fillRule)
      b.u8(s.join)
      b.u8(s.cap)
      b.u8(s.current)
      for (const r of refs) b.u32(r)
      b.u32(s.fill >>> 0)
      b.u32(s.stroke >>> 0)
      b.f32(s.strokeWidth)
      b.f32(s.miterLimit)
      b.f32(s.dashOffset)
      b.f32(s.opacity)
    }
  }
  /** An image node's fit (`FIT`); its bytes go as a payload. */
  imageConfig(id: number, fit: number) {
    this.ops.u8(Op.ImageConfig)
    this.ops.u32(id)
    this.ops.u8(fit)
  }
  cmdFocus(id: number) {
    this.ops.u8(Op.Command)
    this.ops.u32(id)
    this.ops.u8(Cmd.Focus)
  }
  cmdBlur(id: number) {
    this.ops.u8(Op.Command)
    this.ops.u32(id)
    this.ops.u8(Cmd.Blur)
  }
  cmdSetText(id: number, text: string) {
    const s = this.strRef(text)
    this.ops.u8(Op.Command)
    this.ops.u32(id)
    this.ops.u8(Cmd.SetText)
    this.ops.u32(s)
  }
  cmdScrollTo(id: number, x: number, y: number) {
    this.ops.u8(Op.Command)
    this.ops.u32(id)
    this.ops.u8(Cmd.ScrollTo)
    this.ops.f32(x)
    this.ops.f32(y)
  }
  /** Replaces the selection of the focused input, when it is `id` or
   * inside it, with `text` (a paste claim's answer). */
  cmdInsertText(id: number, text: string) {
    const s = this.strRef(text)
    this.ops.u8(Op.Command)
    this.ops.u32(id)
    this.ops.u8(Cmd.InsertText)
    this.ops.u32(s)
  }
  /** Writes plain text to the clipboard; `id` may be NIL. */
  cmdWriteClipboard(id: number, text: string) {
    const s = this.strRef(text)
    this.ops.u8(Op.Command)
    this.ops.u32(id)
    this.ops.u8(Cmd.WriteClipboard)
    this.ops.u32(s)
  }

  listConfig(id: number, overscan: number, fallback: number, templates: readonly ListTemplate[]) {
    const b = this.ops
    b.u8(Op.ListConfig)
    b.u32(id)
    b.f32(overscan)
    b.f32(fallback)
    b.u16(templates.length)
    for (const t of templates) {
      b.f32(t.base ?? 0)
      b.f32(t.inset ?? 0)
      b.f32(t.fontSize ?? 0)
    }
  }
  /** Replaces items `at..at + remove` with `items` (11 bytes each). */
  listSplice(id: number, at: number, remove: number, items: readonly ItemDesc[]) {
    const b = this.ops
    b.u8(Op.ListSplice)
    b.u32(id)
    b.u32(at)
    b.u32(remove)
    b.u32(items.length)
    b.reserve(items.length * 11)
    for (const d of items) {
      b.u16(d.template ?? 0)
      b.u32(d.textLength ?? 0)
      b.u32(d.id ?? NIL)
      b.u8(d.unchanged ? 1 : 0)
    }
  }
  /** Tags a list row with its item index (NIL clears). */
  listIndex(id: number, index: number) {
    this.ops.u8(Op.ListIndex)
    this.ops.u32(id)
    this.ops.u32(index)
  }
  scrollAnchor(id: number, anchor: ScrollAnchor) {
    this.ops.u8(Op.ScrollAnchor)
    this.ops.u32(id)
    this.ops.u8(ANCHOR[anchor])
  }

  /** Replaces a node's declared transitions (none: clears). */
  transition(id: number, transitions: Transitions | undefined) {
    const b = this.ops
    const list = (Object.keys(ANIM_PROP) as AnimProp[]).filter(p => transitions?.[p] !== undefined)
    // Every timing checked before a byte is written: a bad one throws
    // with the op buffer unchanged.
    const timings = list.map(p => timingValues(transitions![p]!))
    b.u8(Op.Transition)
    b.u32(id)
    b.u8(list.length)
    list.forEach((p, i) => {
      b.u8(ANIM_PROP[p])
      putTiming(b, timings[i]!)
    })
  }

  /** Tweens one property to `value`, in the property's wire shape:
   * transform 6 numbers, a color as 0xRRGGBBAA, padding [left, right,
   * top, bottom], gap [column, row], others one number. */
  animate(id: number, prop: AnimProp, value: readonly number[], timing: Timing) {
    const b = this.ops
    const code = ANIM_PROP[prop]
    const want = [6, 1, 1, 1, 1, 1, 4, 2, 1][code]
    if (want === undefined || value.length !== want || !value.every(Number.isFinite)) {
      throw Error(`bad ${String(prop)} animation target`)
    }
    const t = timingValues(timing)
    b.u8(Op.Animate)
    b.u32(id)
    b.u8(code)
    if (prop === "backgroundColor" || prop === "borderColor" || prop === "color") b.u32(value[0]! >>> 0)
    else for (const v of value) b.f32(v)
    putTiming(b, t)
  }

  /** Sets scope `id`'s app state bits (`STATE_BIT`, custom bits); the
   * node becomes a scope. Input bits (hover, pressed, focus) are
   * native's. */
  states(id: number, bits: bigint) {
    this.ops.u8(Op.States)
    this.ops.u32(id)
    this.ops.u64(bits)
  }

  /** Replaces a node's variant table; none removes it and restores the
   * values the node's own props set. */
  variants(id: number, variants: readonly VariantIn[]) {
    const b = this.ops
    b.u8(Op.Variants)
    b.u32(id)
    b.u16(variants.length)
    for (const v of variants) {
      b.u8(v.terms.length)
      b.u8(v.env)
      for (const t of v.terms) {
        b.u32(t.scope)
        b.u64(t.mask)
      }
      const x = v.values
      const has = (k: keyof VariantValues) => x[k] !== undefined
      b.u8(
        (has("fill") ? VALUE_FIELD.FILL : 0) |
          (has("borderColor") ? VALUE_FIELD.BORDER_COLOR : 0) |
          (has("radius") ? VALUE_FIELD.RADIUS : 0) |
          (has("color") ? VALUE_FIELD.COLOR : 0) |
          (has("opacity") ? VALUE_FIELD.OPACITY : 0) |
          (has("transform") ? VALUE_FIELD.TRANSFORM : 0) |
          (has("layout") ? VALUE_FIELD.LAYOUT : 0) |
          (has("borderWidth") ? VALUE_FIELD.BORDER_WIDTH : 0),
      )
      if (x.fill !== undefined) b.u32(x.fill >>> 0)
      if (x.borderColor !== undefined) b.u32(x.borderColor >>> 0)
      if (x.radius !== undefined) b.f32(x.radius)
      if (x.color !== undefined) { b.u8(x.color === null ? 0 : 1); b.u32((x.color ?? 0) >>> 0) }
      if (x.opacity !== undefined) b.f32(x.opacity)
      if (x.transform !== undefined) for (const m of x.transform) b.f32(m)
      if (x.layout !== undefined) {
        const keys = layoutKeys(x.layout)
        b.u64(keys)
        putStyleFields(b, x.layout, keyFields(keys))
      }
      if (x.borderWidth !== undefined) b.f32(x.borderWidth)
    }
  }

  /** The window-width breakpoints of `narrow` and `compact` (logical
   * points, inclusive). */
  environment(narrowMax: number, compactMax: number) {
    this.ops.u8(Op.Environment)
    this.ops.f32(narrowMax)
    this.ops.f32(compactMax)
  }

  /** Sets (or with `null` clears) the color a node's text, inputs and
   * `currentColor` drawings inherit. */
  color(id: number, color: number | null) {
    this.ops.u8(Op.Color)
    this.ops.u32(id)
    this.ops.u8(color === null ? 0 : 1)
    this.ops.u32((color ?? 0) >>> 0)
  }

  /** Seals the transaction and resets every table for the next one. */
  finish(seq: number | bigint): Uint8Array {
    const w = this.out
    w.at = 0
    w.u32(MAGIC)
    w.u16(VERSION)
    w.u16(0)
    w.u64(seq)
    w.u32(this.strings.length)
    w.u32(this.styleCount)
    w.u32(this.spanCount)
    for (const s of this.strings) w.str(s)
    for (const part of [this.styleBytes, this.spanBytes, this.ops]) {
      w.reserve(part.at)
      w.bytes.set(part.bytes.subarray(0, part.at), w.at)
      w.at += part.at
    }
    const buf = w.bytes.slice(0, w.at)
    this.ops.at = 0
    this.styleBytes.at = 0
    this.spanBytes.at = 0
    this.strings.length = 0
    this.stringIx.clear()
    this.styleIx.clear()
    this.spanIx.clear()
    this.styleCount = 0
    this.spanCount = 0
    return buf
  }
}
