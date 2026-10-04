// Commit-side host bookkeeping: JS owns node ids, maps element props to
// CRW2 ops, and seals one transaction per commit via queueMicrotask.
//
// Ids recycle immediately on removal. Native bumps a slot's generation
// when it frees it; JS mirrors the counter, and events carry the
// generation, so an event for a previous occupant of an id is dropped.

import {
  ACTIVATE_SOURCE,
  CHORD_FLAG,
  CLAIM_KIND,
  CUSTOM_STATES,
  DECORATION,
  ENV_BIT,
  type TextAlign,
  MAX_SHADOWS,
  shadowsIn,
  type ShadowIn,
  type BorderSidesIn,
  STATE_BIT,
  Encoder,
  EVENT_KIND,
  WINDOW_BIT,
  EVENT_MASK,
  FIT,
  INTERACTION,
  NIL,
  PRESS_FLAG,
  PRESS_PHASE,
  REPORTED,
  ROLE,
  SUBMIT_KEY,
  parseChord,
  type Claim,
  type SubmitKey,
  layoutPart,
  styleKey,
  partsOf,
  rotateTarget,
  scaleTarget,
  styleParts,
  transformMatrix,
  translateTarget,
  type AccessibilityRole,
  type AnimProp,
  type AnimationEnd,
  type ImageFit,
  ANIM_PROP,
  ANIMATION_TRIGGER,
  END_REASON,
  type Timing,
  type Transitions,
  type ItemDesc,
  type ListTemplate,
  type ScrollAnchor,
  type StyleProps,
  type TextSpanIn,
  type VariantIn,
  type VariantValues,
} from "./wire.js"
import { animationList, FILL, transitionsIn } from "./motion.js"

// 0 view, 1 text, 2 input, 3 surface, 4 list, 5 vector, 7 image — mirror
// NodeKind (6 is reserved)
export type Kind = 0 | 1 | 2 | 3 | 4 | 5 | 7
export const KIND: Record<string, Kind> = {
  view: 0, text: 1, input: 2, surface: 3, list: 4, vector: 5, image: 7,
}

/** One decoded UI -> JS event record (see events.rs `UiEvent`). */
export interface UiEvent {
  kind: number
  /** Native node id. */
  node: number
  /** The node's generation when the event fired. */
  generation: number
  /** A text node's paragraph revision on pointer events that carry a
   * span (key bits 16+); 0 otherwise. */
  revision: number
  /** Pointer position in logical points, when applicable. */
  x: number
  y: number
  /** Aux float: button (pointer), dx (wheel), scroll x. */
  a: number
  /** Aux float: dy (wheel), scroll y. */
  b: number
  /** Packed per kind: key records as events.rs `key_bits`, pointer
   * records mods | button << 8 | span << 16, claims kind | index << 8. */
  key: number
  /** Text payload (change/submit). */
  text: string
}

/** A box in logical points. */
export interface LayoutRect {
  x: number
  y: number
  width: number
  height: number
}

/** `onLayout`: the node's border box after layout, relative to its
 * parent's border box (no scroll offset, no transform), as React
 * Native's. */
export interface LayoutEvt extends LayoutRect {
  target: HostNode
}

/** The window, as native reports it. */
export interface WindowState {
  /** Logical points. */
  width: number
  height: number
  /** Physical pixels per logical point. */
  scale: number
  /** The window has keyboard focus. */
  focused: boolean
  /** Shown: not minimized and not fully covered. */
  visible: boolean
  /** The system appearance is dark. */
  dark: boolean
}

/** A presented frame (`presented`, `capture`): its number since start
 * and its size in pixels. */
export interface Presented {
  frame: number
  width: number
  height: number
}

/** A key a node claims (`keymap`): native skips its own handling of the
 * chord (text entry, Tab, Enter, Escape) and JS runs `run`. The first
 * matching claim on the focused node, then its ancestors, wins. */
export interface KeyClaim {
  /** A chord such as `mod+shift+o`, `escape`, `shift+?` (wire.ts
   * `parseChord`); `mod` is Cmd on Apple platforms and Ctrl elsewhere. */
  keys: string
  run: () => void
  /** The claim applies only while true (default); a false claim lets
   * the key through. */
  when?: boolean
  /** Whether the key's auto-repeat runs it again (default); with
   * `false` repeats are swallowed. */
  repeat?: boolean
}

/** A window-level shortcut (`useHotkeys`): matched after every claim on
 * the focus path. */
export interface Hotkey extends KeyClaim {
  /** Also while a text input has focus. */
  allowInInput?: boolean
}

/** The event of a clipboard claim (`onPaste`, `onCopy`, `onCut`): the
 * clipboard's plain text on paste, the selected text on copy and cut. */
export interface ClipboardEvt {
  target: HostNode
  text: string
}
/** Files dropped on a node that claims drops (`onDrop`). */
export interface DropEvt {
  target: HostNode
  x: number
  y: number
  paths: string[]
}
/** An image decoded (`onLoad`): its natural size in pixels. */
export interface ImageLoadEvt {
  target: HostNode
  width: number
  height: number
}
/** An image failed to load or decode (`onError`). */
export interface ImageErrorEvt {
  target: HostNode
  message: string
}
/** A context-menu request (`onContextMenu`): a secondary press at the
 * pointer, or the ContextMenu key or Shift+F10 at the focused node's
 * center. */
export interface ContextMenuEvt {
  target: HostNode
  x: number
  y: number
}

/** A claim set as sent (claims.rs): its signature and version, and the
 * handlers of each version native may still name — the current one, and
 * older ones until native acks the transaction that replaced them. */
interface ClaimState {
  sig: string
  version: number
  handlers: Map<number, readonly Function[]>
}

/** Claims and their handlers, in claim order. */
interface Declared {
  claims: Claim[]
  handlers: Function[]
}

const APPLE = typeof process !== "undefined" && process.platform === "darwin"

const warned = new Set<string>()
/** Logs a bad prop once: a typo should not take the app down, nor
 * flood the console on every render. */
export function warnOnce(msg: string) {
  if (warned.has(msg)) return
  warned.add(msg)
  console.error(`craie: ${msg}`)
}

/** Props that claim an event kind, in claim order after the keymap. */
const CLAIM_PROPS = [
  ["onPaste", CLAIM_KIND.paste],
  ["onCopy", CLAIM_KIND.copy],
  ["onCut", CLAIM_KIND.cut],
  ["onDrop", CLAIM_KIND.drop],
  ["onContextMenu", CLAIM_KIND.contextMenu],
] as const

function keyClaims(list: readonly Hotkey[] | undefined, out: Declared, window: boolean) {
  for (const k of list ?? []) {
    if (k.when === false) continue
    const c = parseChord(k.keys, APPLE)
    if (!c) {
      warnOnce(`unknown key chord "${k.keys}"`)
      continue
    }
    if (k.repeat === false) c.flags |= CHORD_FLAG.noRepeat
    if (window && k.allowInInput) c.flags |= CHORD_FLAG.inInput
    out.claims.push(c)
    out.handlers.push(k.run)
  }
}

/** A node's declared claims: its keymap, then its claim props. */
function declaredClaims(props: Record<string, any>): Declared {
  const out: Declared = { claims: [], handlers: [] }
  keyClaims(props.keymap, out, false)
  for (const [name, kind] of CLAIM_PROPS) {
    if (typeof props[name] !== "function") continue
    out.claims.push({ kind, flags: 0, mods: 0, key: 0 })
    out.handlers.push(props[name])
  }
  return out
}

function claimSig(claims: readonly Claim[]): string {
  return claims.map(c => `${c.kind}.${c.flags}.${c.mods}.${c.key}`).join()
}

function claimsDeclared(props: Record<string, any>): boolean {
  return props.keymap !== undefined || CLAIM_PROPS.some(([name]) => props[name] !== undefined)
}

function submitKeyOf(props: Record<string, any>): number {
  if (!props.onSubmit) return SUBMIT_KEY.none
  const key = props.submitKey ?? "enter"
  if (Object.hasOwn(SUBMIT_KEY, key)) return SUBMIT_KEY[key as SubmitKey]
  warnOnce(`unknown submitKey "${key}", using "enter"`)
  return SUBMIT_KEY.enter
}

/** Web `event.code` names of the named keys, by code (`KEY_CODE`). */
const NAMED_CODE = [
  "", "Backspace", "Tab", "Enter", "Escape", "ArrowLeft", "ArrowUp", "ArrowRight", "ArrowDown",
  "Home", "End", "PageUp", "PageDown", "Delete", "Space", "Insert", "ContextMenu",
]
/** Web `event.code` names of the punctuation keys, by US character. */
const PUNCT_CODE: Record<string, string> = {
  "-": "Minus", "=": "Equal", "[": "BracketLeft", "]": "BracketRight", "\\": "Backslash",
  ";": "Semicolon", "'": "Quote", "`": "Backquote", ",": "Comma", ".": "Period", "/": "Slash",
}

/** The web's `event.code` of a key record: the physical key's US
 * character (`physical`), else the named key (`named`), else "". */
function webCode(named: number, physical: number): string {
  if (physical) {
    const c = String.fromCharCode(physical)
    if (c >= "a" && c <= "z") return `Key${c.toUpperCase()}`
    if (c >= "0" && c <= "9") return `Digit${c}`
    return PUNCT_CODE[c] ?? ""
  }
  if (named >= 32 && named <= 55) return `F${named - 31}`
  return NAMED_CODE[named] ?? ""
}

/** A key record (events.rs `key_bits`) as a listener's event. */
function keyEvt(e: { target: HostNode; x: number; y: number }, ev: UiEvent) {
  const named = (ev.key >>> 8) & 0xff
  return {
    ...e,
    key: named,
    char: ev.text,
    shift: !!(ev.key & 1),
    ctrl: !!(ev.key & 2),
    alt: !!(ev.key & 4),
    meta: !!(ev.key & 8),
    repeat: !!(ev.key & 16),
    composing: !!(ev.key & 32),
    code: webCode(named, (ev.key >>> 16) & 0xff),
  }
}

/** Item identity bookkeeping of a list node: React keys interned to
 * u32 ids (the wire's item identity) and the splices sent. */
export interface ListKeys {
  ids: Map<unknown, number>
  keys: Map<number, unknown>
  next: number
  /** Splices sent; native counts the same and stamps range events. */
  revision: number
}

export interface HostNode {
  id: number
  /** Generation of `id` this node occupies (mirrors native). */
  gen: number
  type: string
  kind: Kind
  props: Record<string, any>
  mounted: boolean
  root: CraieHost
  /** Hidden by React (Suspense): sent as `display: none`. */
  suspended: boolean
  /** Children appended before this node got an id; replayed on materialize. */
  initial: HostNode[]
  /** List nodes only. */
  list?: ListKeys
  /** Layer containers only: the layer it was opened from (`outer`,
   * null: none), the node that owns it natively (a FocusTrap's, else
   * `outer`), the children React placed in it, and the open layers
   * opened from it. Open while it has children or open layers. */
  layer?: {
    outer: HostNode | null
    owner: OwnerRef | null
    kids: Set<HostNode>
    owned: Set<HostNode>
  }
  /** Text nodes: nested text nodes, in order. They have no native node
   * (virtual); their text and style become spans of the root's
   * paragraph. */
  textKids?: HostNode[]
  /** A virtual text node's parent text node. */
  textParent?: HostNode
  /** A text root: the node that owns each span it last sent (event
   * routing), and what it last sent. */
  spanOwners?: HostNode[]
  /** A text root: who heard the press in progress (`onPressIn`), so its
   * out or cancel reaches it even across a new span table. */
  pressOwner?: HostNode
  sentParagraph?: string
  /** A text root: the line limit last sent (0: none). */
  sentLines?: number
  sentInteraction?: string
  /** Vector nodes: the drawing last sent, as JSON of [viewBox, shapes],
   * "" for none; undefined after an asset. */
  sentDrawing?: string
  /** Paragraph ops sent for this node, wrapping at 2^32 (mirrors native
   * `Paragraph::revision`): a span event from another revision was
   * hit-tested against an older span table. */
  paragraphRev?: number
  /** The claim set sent for this node (`keymap`, `onPaste`...). */
  claims?: ClaimState
  /** The state bits this scope last sent. */
  sentBits?: bigint
  /** The signature of the variant table last sent ("" none). */
  sentVariants?: string
  /** Each variant path seen (`_hover._selected`) and its block number:
   * its animations' identity, kept while the node lives, so blocks
   * coming and going (`_busy: busy && {...}`) move no other. */
  variantBlocks?: Map<string, number>
  /** The transitions last sent, after the reduced-motion policy ("":
   * none). */
  sentTransitions?: string
  /** The `animation` list last sent, after the policy, and whether it
   * reports ends ("": none). */
  sentAnimation?: string
  /** The native parent it was last placed under (null: the root level
   * or a layer's; unset once detached without an exit). */
  parent?: HostNode | null
  /** An exit's root: its exit, running or ended. */
  exit?: Exit
  /** `animate` calls not ended yet, oldest first (native keeps one tween
   * per property, so ends arrive in call order per property). */
  pendingAnims?: { prop: number; resolve: (end: AnimationEnd) => void }[]
  focus(): void
  blur(): void
  scrollTo(x: number, y: number): void
  /** Replaces an input node's buffer. Inputs are uncontrolled: this
   * command is the only way to change the text after mount. */
  setText(text: string): void
  /** Tweens one property natively to `to` (the value it keeps after,
   * until a commit sets that property again): transform an RN
   * transform list, translate a length or [x, y] ("50%" of the node's
   * own size), rotate degrees or an angle string, scale a factor or
   * [x, y], colors as in props, padding a number or [left, right, top,
   * bottom], gap a number or [column, row]. Each transform part tweens
   * on its own: `animate("rotate", 360, ...)` turns once while a
   * variant scales the same node. Resolves when
   * the tween ends: finished, cancelled (a commit set the property),
   * retargeted (another tween replaced it), or removed (with its
   * node). */
  animate(prop: AnimProp, to: unknown, timing: Timing): Promise<AnimationEnd>
  /** The node's window-space bounding box (logical points, scroll
   * offsets and transforms applied), from the layout that follows the
   * commits made so far; `null` when it is gone or not displayed. */
  measure(): Promise<LayoutRect | null>
}

export interface Transport {
  send(frame: Uint8Array): void
  close(reason?: string): void
  /** Native acks an applied transaction by seq; resolves `flush()`. */
  onAck?(cb: (seq: number) => void): void
  /** Native pushes UI events (pointer/key/focus/input/scroll). */
  onEvent?(cb: (ev: UiEvent) => void): void
}

/** Native bounds node ids (they index dense stores): mirror host.rs
 * `MAX_NODES`. */
const MAX_ID = 1 << 24
/** The largest font file `registerFont` sends: half the session's 4 MiB
 * commit queue, so commits React submits while the UI thread drains a
 * font still fit. */
export const MAX_FONT_BYTES = 2 * 1024 * 1024

/** "#rgb" / "#rrggbb" / "#rrggbbaa" / "rgb(r, g, b)" / "rgba(r, g,
 * b, a)" (the kit's formats; also space-separated with "/ a",
 * percentages, any case and surrounding space, as CSS) / number ->
 * 0xRRGGBBAA. */
export function color(v: string | number | undefined, fallback = 0): number {
  if (v === undefined) return fallback
  if (typeof v === "number") return v >>> 0
  const t = v.trim()
  if (/^rgba?\(/i.test(t)) return rgbFunction(t)
  let s = t.startsWith("#") ? t.slice(1) : t
  if (s.length === 3) s = [...s].map(c => c + c).join("") + "ff"
  if (s.length === 6) s += "ff"
  if (s.length !== 8) throw Error(`bad color "${v}"`)
  const n = parseInt(s, 16)
  if (!Number.isFinite(n)) throw Error(`bad color "${v}"`)
  return n >>> 0
}

function rgbFunction(v: string): number {
  const m = /^rgba?\(([^)]*)\)$/i.exec(v.trim())
  const parts = m ? m[1]!.trim().split(/\s*[,/]\s*|\s+/) : []
  if (parts.length !== 3 && parts.length !== 4) throw Error(`bad color "${v}"`)
  const channel = (p: string, max: number) => {
    const pct = p.endsWith("%")
    const n = Number(pct ? p.slice(0, -1) : p)
    if (p === "" || !Number.isFinite(n)) throw Error(`bad color "${v}"`)
    return Math.round(Math.min(1, Math.max(0, pct ? n / 100 : n / max)) * 255)
  }
  const [r, g, b] = parts.slice(0, 3).map(p => channel(p, 255)) as [number, number, number]
  const a = parts.length === 4 ? channel(parts[3]!, 1) : 255
  return ((r << 24) | (g << 16) | (b << 8) | a) >>> 0
}

function textOf(props: Record<string, any>): string {
  if (typeof props.text === "string") return props.text
  if (typeof props.children === "string") return props.children
  if (typeof props.children === "number") return String(props.children)
  return ""
}

/** Pointer event kind -> the listener prop a nested Text may hold. */
const POINTER_HANDLER: Record<number, string> = {
  [EVENT_KIND.pointerMove]: "onPointerMove",
  [EVENT_KIND.pointerDown]: "onPointerDown",
  [EVENT_KIND.pointerUp]: "onPointerUp",
}

const isHigh = (c: number) => c >= 0xd800 && c < 0xdc00
const isLow = (c: number) => c >= 0xdc00 && c < 0xe000

/** UTF-8 byte length of `s` as the encoder writes it (span starts are
 * byte offsets natively): a surrogate pair is 4 bytes, a lone surrogate
 * becomes U+FFFD, 3 bytes. */
export function utf8Length(s: string): number {
  let n = 0
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i)
    if (c < 0x80) n += 1
    else if (c < 0x800) n += 2
    else if (isHigh(c) && i + 1 < s.length && isLow(s.charCodeAt(i + 1))) { n += 4; i++ }
    else n += 3
  }
  return n
}

/** Key of a transition declaration, for change detection. */
function transitionKey(t: Transitions | undefined): string {
  if (!t) return ""
  const keys = Object.keys(t).filter(k => t[k as AnimProp] !== undefined).sort()
  return keys.length ? JSON.stringify(keys.map(k => [k, t[k as AnimProp]])) : ""
}

/** An `animate` target in its wire shape (see `Encoder.animate`). */
function animValue(prop: AnimProp, to: unknown): number[] {
  const nums = (v: unknown, n: number): number[] => {
    if (typeof v === "number") return Array(n).fill(v)
    if (Array.isArray(v) && v.length === n && v.every(x => typeof x === "number")) return v
    throw Error(`bad ${prop} animation target`)
  }
  switch (prop) {
    case "transform": return [...transformMatrix(to as any)]
    case "translate": return translateTarget(to)
    case "rotate": return [rotateTarget(to)]
    case "scale": return scaleTarget(to)
    case "backgroundColor": case "borderColor": case "color": return [color(to as string | number)]
    case "padding": return nums(to, 4)
    case "gap": return nums(to, 2)
    default: return nums(to, 1)
  }
}

/** A nested Text's span style: its parent's, with the props it sets
 * itself. Line height is the paragraph's (the root's) only. */
function inheritSpan(parent: TextSpanIn, props: Record<string, any>): TextSpanIn {
  const own = spanStyle(props)
  return {
    start: 0,
    fontSize: props.fontSize !== undefined ? own.fontSize : parent.fontSize,
    color: props.color !== undefined ? color(props.color) : parent.color,
    inheritColor: props.color === undefined && parent.inheritColor,
    weight: props.fontWeight !== undefined ? own.weight : parent.weight,
    italic: props.fontStyle !== undefined ? own.italic : parent.italic,
    fontFamily: props.fontFamily !== undefined ? own.fontFamily : parent.fontFamily,
    decoration: props.textDecorationLine !== undefined ? own.decoration : parent.decoration,
    letterSpacing: props.letterSpacing !== undefined ? own.letterSpacing : parent.letterSpacing,
    tabular: props.fontVariant !== undefined ? own.tabular : parent.tabular,
    lineHeight: parent.lineHeight,
    align: parent.align,
    // Presses on a pressable nested Text's span, or a span inside it,
    // go to it (`spanTarget`).
    pressable: isPressable(props) || !!parent.pressable,
  }
}

/** Span zero of a text node: the base style. */
function decorationOf(line: unknown): number {
  if (typeof line !== "string") return 0
  return (line.includes("underline") ? DECORATION.underline : 0) |
    (line.includes("line-through") ? DECORATION.lineThrough : 0)
}

/** The span a Text's own props describe (its base style). Its color is
 * inherited: the Text's own `color` travels as COLOR, so a new color
 * (or a variant's, or a tween) leaves the paragraph alone; white when
 * nothing up the tree sets one. */
export function spanStyle(props: Record<string, any>): TextSpanIn {
  return {
    start: 0,
    fontSize: props.fontSize ?? 14,
    color: 0xffff_ffff,
    inheritColor: true,
    weight: typeof props.fontWeight === "number"
      ? props.fontWeight
      : props.fontWeight === "bold" ? 700 : 400,
    italic: props.fontStyle === "italic",
    fontFamily: props.fontFamily,
    decoration: decorationOf(props.textDecorationLine),
    letterSpacing: props.letterSpacing ?? 0,
    lineHeight: props.lineHeight ?? 0,
    tabular: tabularOf(props.fontVariant),
    align: alignOf(props.textAlign),
  }
}

/** `numberOfLines` as native takes it: a whole number of lines in
 * [0, 65535]; anything else is no limit. */
function linesOf(n: unknown): number {
  return typeof n === "number" && Number.isFinite(n) && n > 0 ? Math.min(0xffff, Math.floor(n)) : 0
}

/** React Native's `fontVariant`: the last of `tabular-nums` and
 * `proportional-nums` wins. */
function tabularOf(variant: unknown): boolean {
  if (!Array.isArray(variant)) return false
  let on = false
  for (const v of variant) {
    if (v === "tabular-nums") on = true
    else if (v === "proportional-nums") on = false
  }
  return on
}

function alignOf(align: unknown): TextAlign {
  if (align === undefined || align === "auto") return "auto"
  if (align === "left" || align === "center" || align === "right") return align
  warnOnce(`textAlign "${String(align)}" is drawn as "auto"`)
  return "auto"
}

function sameSpan(a: TextSpanIn, b: TextSpanIn): boolean {
  return a.fontSize === b.fontSize && a.color === b.color && !!a.inheritColor === !!b.inheritColor &&
    a.weight === b.weight && !!a.italic === !!b.italic &&
    a.fontFamily === b.fontFamily && (a.decoration ?? 0) === (b.decoration ?? 0) &&
    (a.letterSpacing ?? 0) === (b.letterSpacing ?? 0) && (a.lineHeight ?? 0) === (b.lineHeight ?? 0) &&
    !!a.pressable === !!b.pressable && !!a.tabular === !!b.tabular && (a.align ?? "auto") === (b.align ?? "auto")
}

// Listener prop name -> mask bit; emit INTERACTION when the mask changes.
const LISTENERS: Record<string, number> = {
  onPointerMove: EVENT_MASK.pointerMove,
  onPointerDown: EVENT_MASK.pointerDown,
  onPointerUp: EVENT_MASK.pointerUp,
  onPointerEnter: EVENT_MASK.pointerEnterLeave,
  onPointerLeave: EVENT_MASK.pointerEnterLeave,
  onWheel: EVENT_MASK.wheel,
  onKeyDown: EVENT_MASK.key,
  onKeyUp: EVENT_MASK.key,
  onFocus: EVENT_MASK.focus,
  onBlur: EVENT_MASK.focus,
  onChangeText: EVENT_MASK.input,
  onSubmit: EVENT_MASK.input,
  onScroll: EVENT_MASK.scroll,
  onPress: EVENT_MASK.activate,
  onPressIn: EVENT_MASK.press,
  onPressOut: EVENT_MASK.press,
  onLayout: EVENT_MASK.layout,
}

/** A node that owns presses: a Pressable (`__pressable`), or anything
 * with `onPress` (a Text link). A Text with only `onPressIn` or
 * `onPressOut` is no pressable: it would swallow its row's presses. */
function isPressable(props: Record<string, any>): boolean {
  return !!props.__pressable || typeof props.onPress === "function"
}

/** The node's `PRESS_FLAG` bits: a disabled pressable swallows its
 * presses; `preventFocusOnPress` keeps focus where it is. */
function pressFlags(props: Record<string, any>): number {
  if (!isPressable(props)) return 0
  return PRESS_FLAG.pressable | (props.disabled ? PRESS_FLAG.disabled : 0) |
    (props.preventFocusOnPress ? PRESS_FLAG.keepFocus : 0)
}

/** `INTERACTION` flags of a node's props. */
function interactionFlags(props: Record<string, any>): number {
  return (props.focusable ? INTERACTION.focusable : 0) |
    (props.selectable ? INTERACTION.selectable : 0) |
    (props.inert ? INTERACTION.inert : 0) |
    (props.autoFocus ? INTERACTION.autoFocus : 0) |
    pressFlags(props) << INTERACTION.pressShift
}

function listenerMask(props: Record<string, any>): number {
  let mask = 0
  for (const name in LISTENERS) {
    if (typeof props[name] === "function") mask |= LISTENERS[name]!
  }
  return mask
}

/** The layout style a node sends: its style minus spatial keys, with
 * `display: none` when the node is hidden (prop or Suspense). */
function layoutOf(props: Record<string, any>, suspended: boolean): StyleProps | undefined {
  const base = layoutPart(props.style)
  if (!(props.hidden || suspended)) return base
  return { ...base, display: "none" }
}

/** A style's `zIndex` as the i32 native sorts by, 0 when unset. Like
 * React Native, any number goes: rounded and clamped, NaN is 0. */
function zOf(style: StyleProps | undefined): number {
  const z = style?.zIndex ?? 0
  if (Number.isInteger(z) && z >= -0x8000_0000 && z <= 0x7fff_ffff) return z
  const i = Number.isNaN(z) ? 0 : Math.min(Math.max(Math.round(z), -0x8000_0000), 0x7fff_ffff)
  warnOnce(`zIndex ${z} is not a 32-bit integer, using ${i}`)
  return i
}

function same(a: readonly number[], b: readonly number[]): boolean {
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false
  return true
}

function roleOf(props: Record<string, any>): number {
  const r = props.accessibilityRole as AccessibilityRole | undefined
  if (r === undefined) return ROLE.none
  const v = ROLE[r]
  if (v === undefined) throw Error(`unknown accessibilityRole "${r}"`)
  return v
}

/** The states a scope reports while false: `expanded` and `selected`
 * when the prop was given, so `expanded={false}` is collapsed and a
 * plain button is neither. (`checked` goes with the check roles, and
 * native reports `selected` on selectable roles only.) */
function reportedOf(props: Record<string, any>): number {
  if (!props.__scope) return 0
  return (props.expanded !== undefined ? REPORTED.expanded : 0) |
    (props.selected !== undefined ? REPORTED.selected : 0)
}

function f32bits(v: number): number {
  const b = new DataView(new ArrayBuffer(4))
  b.setFloat32(0, v, true)
  return b.getUint32(0, true)
}

/** Surface parameters: colors as strings or numbers, floats as f32 bits
 * when tagged `{ f32 }`. */
export type SurfaceParam = number | string | { f32: number }
function surfaceParams(params: readonly SurfaceParam[] | undefined): number[] {
  const out = [0, 0, 0, 0]
  for (let i = 0; i < 4; i++) {
    const p = params?.[i]
    if (p === undefined) continue
    out[i] = typeof p === "object" ? f32bits(p.f32) : color(p)
  }
  return out
}

/** Native frame statistics, about twice a second while frames are drawn
 * (none while idle). */
export interface FrameStats {
  /** Frames drawn per second. */
  fps: number
  /** Mean CPU time per frame: layout, scene, upload, and draw encoding
   * (ms). */
  cpuMs: number
  /** The largest CPU time of one frame in the window (ms). */
  maxCpuMs: number
  /** Mean layout and scene time per frame (ms). */
  prepareMs: number
  /** Live native nodes. */
  nodes: number
  /** Running native tweens (declared transitions and `animate` calls). */
  tweens: number
}

const frameStatsListeners = new Set<(s: FrameStats) => void>()

/** Calls `listener` with each native frame-statistics report; returns
 * the unsubscribe function. */
export function onFrameStats(listener: (s: FrameStats) => void): () => void {
  frameStatsListeners.add(listener)
  return () => {
    frameStatsListeners.delete(listener)
  }
}

/** A scope: an element whose state bits variants read (Pressable, a
 * View with `group`). `node` is its host node, once created. */
export interface ScopeRef {
  node: HostNode | null
}
/** What owns the layers opened below an element (`Layer`,
 * `FocusTrap`): `node` is its host node, once created. */
export interface OwnerRef {
  node: HostNode | null
}
/** The scopes an element sees, nearest first (itself, when a scope). */
export interface ScopeChain {
  ref: ScopeRef
  /** The `group` name: `_name` keys address this scope. */
  name?: string
  parent: ScopeChain | null
}

/** Custom states by name -> bit, in declaration order. */
const customStates = new Map<string, number>()

/** Declares custom states: each name gets the next free bit (54 at
 * most). A custom state ranks below every built-in one, and a later
 * one above an earlier one, when two variants tie on depth. */
export function defineStates(names: readonly string[]) {
  for (const name of names) {
    if (customStates.has(name)) continue
    if (name in STATE_BIT || name in ENV_BIT) throw Error(`state "${name}" is built in`)
    if (customStates.size === CUSTOM_STATES) throw Error(`more than ${CUSTOM_STATES} custom states`)
    customStates.set(name, customStates.size)
  }
}

/** The state props a scope sets itself; the others are native's. */
const APP_STATES = ["expanded", "selected", "checked", "highlighted", "disabled"] as const

function stateBit(name: string): number | undefined {
  return Object.hasOwn(STATE_BIT, name) ? STATE_BIT[name as keyof typeof STATE_BIT] : customStates.get(name)
}

/** A scope's app state bits: its built-in state props and `states`. */
function stateBits(props: Record<string, any>): bigint {
  let bits = 0n
  for (const s of APP_STATES) if (props[s]) bits |= 1n << BigInt(STATE_BIT[s])
  const custom = props.states as Record<string, boolean> | undefined
  for (const name in custom) {
    const bit = customStates.get(name)
    if (bit === undefined) warnOnce(`unknown state "${name}" (declare it with defineStates)`)
    else if (custom[name]) bits |= 1n << BigInt(bit)
  }
  return bits
}

function hasVariantKeys(props: Record<string, any>): boolean {
  for (const k in props) if (k[0] === "_" && k[1] !== "_" && props[k]) return true
  return false
}

/** A variant with its scopes unresolved: its `_` path (`_hover._selected`),
 * the path's terms and environment, and the block of values at its end. */
interface PathVariant {
  path: string
  terms: Map<ScopeRef, bigint>
  env: number
  block: Record<string, any>
}

/** Flattens `_` keys depth first, in declaration order: nesting ANDs.
 * A key names a state (of the scope in effect: the nearest, or one a
 * scope key picked), an environment bit, or a `group` up the chain. */
function flattenVariants(
  props: Record<string, any>,
  chain: ScopeChain | null,
  out: PathVariant[] = [],
  terms = new Map<ScopeRef, bigint>(),
  env = 0,
  scope = chain,
  prefix = "",
): PathVariant[] {
  for (const key in props) {
    const block = props[key]
    if (key[0] !== "_" || key[1] === "_" || !block || typeof block !== "object") continue
    const name = key.slice(1)
    let t = terms, e = env, s = scope
    const bit = stateBit(name)
    if (bit !== undefined) {
      if (!scope) {
        warnOnce(`${key} needs a scope: a Pressable or a View with group above, inside any Portal or Layer`)
        continue
      }
      t = new Map(terms)
      t.set(scope.ref, (t.get(scope.ref) ?? 0n) | (1n << BigInt(bit)))
    } else if (Object.hasOwn(ENV_BIT, name)) {
      e |= ENV_BIT[name as keyof typeof ENV_BIT]
    } else {
      let c = chain
      while (c && c.name !== name) c = c.parent
      if (!c) {
        warnOnce(`unknown variant key "${key}": not a state or environment, and no group "${name}" above`
          + " (a Portal or Layer starts a new chain)")
        continue
      }
      s = c
    }
    const path = prefix + key
    out.push({ path, terms: t, env: e, block })
    flattenVariants(block, chain, out, t, e, s, path + ".")
  }
  return out
}

/** Variant block keys that apply; `_` keys nest. */
const VARIANT_KEYS = new Set([
  "backgroundColor", "borderColor", "borderWidth", "borderRadius", "boxShadow", "color", "style", "animation",
])

/** A box shadow as React Native's structured `boxShadow` takes it:
 * offsets, blur and spread in points; the first listed paints on top. */
export interface BoxShadow {
  offsetX?: number
  offsetY?: number
  blurRadius?: number
  spreadDistance?: number
  color: string | number
  inset?: boolean
}

/** `boxShadow` in wire form; past `MAX_SHADOWS` the rest is dropped. */
function shadowList(list: readonly BoxShadow[] | undefined): ShadowIn[] {
  if (!list?.length) return []
  if (list.length > MAX_SHADOWS) warnOnce(`boxShadow holds at most ${MAX_SHADOWS} shadows`)
  return shadowsIn(list.map(s => ({
    x: s.offsetX ?? 0,
    y: s.offsetY ?? 0,
    blur: s.blurRadius ?? 0,
    spread: s.spreadDistance ?? 0,
    color: color(s.color),
    inset: !!s.inset,
  })))
}

const SIDE_NAMES = ["Top", "Right", "Bottom", "Left"] as const

/** React Native's per-side borders (`borderTopWidth`... `borderLeftColor`),
 * each side falling back to `borderWidth` and `borderColor`; `undefined`
 * when no per-side prop is set (the uniform border paints). */
function sidesOf(props: Record<string, any>): BorderSidesIn | undefined {
  if (!SIDE_NAMES.some(s => props[`border${s}Width`] !== undefined || props[`border${s}Color`] !== undefined)) {
    return undefined
  }
  const widths = SIDE_NAMES.map(s => props[`border${s}Width`] ?? props.borderWidth ?? 0)
  const colors = SIDE_NAMES.map(s => color(props[`border${s}Color`] ?? props.borderColor))
  return { widths: widths as unknown as BorderSidesIn["widths"], colors: colors as unknown as BorderSidesIn["colors"] }
}

const sidesKey = (s: BorderSidesIn | undefined) => (s ? `${s.widths.join(",")};${s.colors.join(",")}` : "")

const shadowKey = (list: readonly ShadowIn[]) =>
  list.map(s => `${s.x},${s.y},${s.blur},${s.spread},${s.color},${+s.inset}`).join(";")

/** A variant block's values in wire form (`undefined`: none). Each
 * layout key applies on its own, over the base and less specific
 * variants. A hidden node (`hidden`, Suspense) stays hidden: variants
 * set no `display` on it. */
function variantValues(n: HostNode, block: Record<string, any>, hidden: boolean): VariantValues | undefined {
  const v: VariantValues = {}
  const boxed = n.kind !== 1
  const style: StyleProps | undefined = block.style
  for (const k in block) {
    if (k[0] !== "_" && !VARIANT_KEYS.has(k)) warnOnce(`a variant does not apply "${k}" (LEDGER DF-29)`)
  }
  if (boxed) {
    if (block.backgroundColor !== undefined) v.fill = color(block.backgroundColor)
    if (block.borderColor !== undefined) v.borderColor = color(block.borderColor)
    if (block.borderWidth !== undefined) v.borderWidth = block.borderWidth
    if (block.borderRadius !== undefined) v.radius = block.borderRadius
    if (block.boxShadow !== undefined) v.shadows = shadowList(block.boxShadow)
  } else if (["backgroundColor", "borderColor", "borderWidth", "borderRadius", "boxShadow"].some(k => k in block)) {
    warnOnce("a Text variant sets no box paint: wrap it in a View")
  }
  if (block.color !== undefined) v.color = color(block.color)
  if (style?.opacity !== undefined) v.opacity = style.opacity
  Object.assign(v, styleParts(style))
  if (style?.zIndex !== undefined) warnOnce("a variant does not apply style.zIndex (LEDGER DF-29)")
  const layout = layoutPart(style)
  if (layout) {
    const { transition: _, ...rest } = layout
    if (hidden) delete rest.display
    if (Object.keys(rest).length) v.layout = rest
  }
  return Object.keys(v).length ? v : undefined
}

/** A variant table's signature, for change detection. */
function variantsKey(list: readonly VariantIn[]): string {
  return list.length ? JSON.stringify(list, (_, x) => (typeof x === "bigint" ? x.toString(16) : x)) : ""
}

/** A variant block's motion in wire form, under the reduced-motion
 * setting: its `style.transition` (declared, even empty or `"none"`, it
 * replaces the node's list while the variant holds) and `animation`. */
function variantMotion(n: HostNode, block: Record<string, any>, reduced: boolean): Pick<VariantIn, "transitions" | "animations"> {
  const out: Pick<VariantIn, "transitions" | "animations"> = {}
  const t = transitionsIn(block.style?.transition, reduced)
  if (t) out.transitions = t
  const list = animationList(block.animation, FILL.none, reduced, n.kind !== 1)
  if (list.length) out.animations = list
  return out
}

/** A running or ended exit (`exit` prop): React has deleted its root,
 * native still draws the subtree. Each id React releases meanwhile is
 * parked (kept out of `freeIds`, its generation unchanged) until native
 * frees the subtree and says so (`exitEnd`); after that, a release
 * recycles at once. */
interface Exit {
  node: HostNode
  gen: number
  parked: number[]
  ended: boolean
  /** Unmount cut it short: its end is on its way. */
  cut?: boolean
  /** The layer container it was removed from: open until the end. */
  layer?: HostNode
}

export class CraieHost {
  /** The user asked for reduced motion (the environment event): the
   * motion props' policies (`reducedMotion`) apply. */
  private reducedMotion = false
  private nextId = 0
  private freeIds: number[] = []
  /** Generation per id; bumped when native frees the slot. */
  private gens: number[] = []
  /** Running exits by root id. */
  private exits = new Map<number, Exit>()
  /** In `unmount`: removals start no exit. */
  private unmounting = false
  private seq = 0
  private encoder = new Encoder()
  private scheduled = false
  private flushWaiters = new Map<number, () => void>()
  /** The last font's ack: the next font waits for it (`registerFont`). */
  private fontChain: Promise<void> = Promise.resolve()
  /** Live nodes by native id — the event-dispatch target table. */
  private nodes = new Map<number, HostNode>()
  /** Open layer containers, in open order: the root level's tail. */
  private layers: HostNode[] = []
  /** Claim versions this commit replaced, and replaced versions by the
   * transaction that replaced them: their handlers go once native acks
   * it (it acks after delivering every event raised before). */
  private replaced: { state: ClaimState; version: number }[] = []
  private retired: { seq: number; state: ClaimState; version: number }[] = []
  /** The window list (`useHotkeys`): each hook's bindings, in mount
   * order, and the claim set they make (latest mounted first). */
  private hotkeys = new Map<object, readonly Hotkey[]>()
  private windowClaims: ClaimState = { sig: "", version: 0, handlers: new Map() }
  /** The window list may have changed: the seal sends it (once). */
  private windowDirty = false
  /** Measures and presentations waiting for native, by request. */
  private requests = new Map<number, (ev: UiEvent) => void>()
  private nextRequest = 0
  /** The window's state, once native reported it, and who listens. */
  private windowState: WindowState | null = null
  private windowListeners = new Set<(w: WindowState) => void>()

  /** `inEvent` runs each event's dispatch; the root sets React's
   * update priority for its kind around it. */
  constructor(
    private transport: Transport,
    inEvent: (kind: number, dispatch: () => void) => void = (_, dispatch) => dispatch(),
  ) {
    transport.onAck?.((seq) => this.ack(seq))
    transport.onEvent?.((ev) => inEvent(ev.kind, () => this.dispatchEvent(ev)))
  }

  /** Native applied transaction `seq`: resolve its flush waiters. */
  private ack(seq: number) {
    this.flushWaiters.get(seq)?.()
    this.flushWaiters.delete(seq)
    let done = 0
    while (done < this.retired.length && this.retired[done]!.seq <= seq) {
      const r = this.retired[done++]!
      r.state.handlers.delete(r.version)
    }
    if (done) this.retired.splice(0, done)
  }

  private alloc(): number {
    const free = this.freeIds.pop()
    if (free !== undefined) return free
    if (this.nextId === MAX_ID) throw Error("node ids exhausted")
    this.gens.push(0)
    return this.nextId++
  }

  /** Records ops; schedules the seal once per synchronous commit batch. */
  ready(): boolean {
    if (!this.scheduled) {
      this.scheduled = true
      queueMicrotask(() => {
        this.scheduled = false
        this.seal()
      })
    }
    return true
  }

  private seal() {
    this.flushTexts()
    this.flushVariants()
    this.flushLayers()
    if (this.windowDirty) {
      this.windowDirty = false
      this.sendClaims(NIL, this.windowClaims, this.windowDeclared())
    }
    const seq = ++this.seq
    // Without acks there is no telling when native is done with a
    // version: its handlers go now rather than never.
    for (const r of this.replaced) {
      if (this.transport.onAck) this.retired.push({ seq, ...r })
      else r.state.handlers.delete(r.version)
    }
    this.replaced = []
    this.transport.send(this.encoder.finish(seq))
  }

  /** Sends a claim set when its declaration changed (a new version),
   * and keeps the current version's handlers fresh either way: a press
   * raised before this commit runs this commit's closures, as a DOM
   * listener would once React re-rendered. Call while a transaction is
   * open. */
  private sendClaims(id: number, state: ClaimState, d: Declared) {
    const sig = claimSig(d.claims)
    if (sig === state.sig) {
      if (state.version) state.handlers.set(state.version, d.handlers)
      return
    }
    if (state.version) this.replaced.push({ state, version: state.version })
    state.sig = sig
    state.version = (state.version + 1) >>> 0
    state.handlers.set(state.version, d.handlers)
    this.encoder.claims(id, state.version, d.claims)
  }

  /** `useHotkeys`: sets one hook's bindings (`null` drops them). The
   * window list is every hook's bindings, the latest mounted hook's
   * first, so an overlay's shortcuts beat the page's; the seal sends it
   * when its chords changed. */
  setHotkeys(owner: object, bindings: readonly Hotkey[] | null) {
    if (bindings) this.hotkeys.set(owner, bindings)
    else this.hotkeys.delete(owner)
    const d = this.windowDeclared()
    const w = this.windowClaims
    if (claimSig(d.claims) === w.sig) {
      if (w.version) w.handlers.set(w.version, d.handlers)
    } else if (!this.windowDirty) {
      this.windowDirty = true
      this.ready()
    }
  }

  private windowDeclared(): Declared {
    const d: Declared = { claims: [], handlers: [] }
    for (const list of [...this.hotkeys.values()].reverse()) keyClaims(list, d, true)
    return d
  }

  /** A request id for a native answer, and the promise it resolves. */
  private request<T>(answer: (ev: UiEvent) => T): [number, Promise<T>] {
    const id = this.nextRequest
    this.nextRequest = (this.nextRequest + 1) >>> 0
    const done = new Promise<T>(resolve => this.requests.set(id, ev => resolve(answer(ev))))
    return [id, done]
  }

  /** The window's state (`null` until native reports it). */
  window(): WindowState | null {
    return this.windowState
  }

  /** Calls `listener` on each change of the window's state; returns the
   * unsubscribe function. */
  onWindow(listener: (w: WindowState) => void): () => void {
    this.windowListeners.add(listener)
    return () => {
      this.windowListeners.delete(listener)
    }
  }

  /** Resolves once a frame that includes every commit made so far is on
   * screen (React's commits: after `renderSync`, or once an update has
   * committed; a `render` call may not have yet); with `rest`, the first such frame with nothing moving or
   * loading (no animation, no scroll settling, no image decoding).
   * Never resolves while an animation loops. */
  presented(options: { rest?: boolean } = {}): Promise<Presented> {
    return this.present(null, options.rest ?? false)
  }

  /** `presented`, writing that frame to a PNG at `path` (device
   * pixels). Rejects when the capture fails. */
  capture(path: string, options: { rest?: boolean } = {}): Promise<Presented> {
    return this.present(path, options.rest ?? false)
  }

  private present(path: string | null, rest: boolean): Promise<Presented> {
    const [id, done] = this.request(ev => ev)
    this.ready()
    this.encoder.cmdPresent(id, rest, path)
    return done.then(ev => {
      if (ev.text) throw Error(`capture failed: ${ev.text}`)
      return { frame: ev.revision, width: ev.x, height: ev.y }
    })
  }

  /** Registers a font file the app ships (TTF, OTF or a collection;
   * not WOFF or WOFF2) under `family`, else under the file's own family
   * names. Registered families come before the system's for the spans
   * that name them; text already showing in a family reflows when its
   * font arrives. A variable font serves every weight its axis holds.
   * The last registration of a face (family, weight, italic) wins.
   *
   * Resolves once native has the font. A font goes alone, once native
   * has acked everything sent before it (the previous font included),
   * so it has the session's 4 MiB commit queue to itself: text rendered
   * meanwhile shows in a fallback face, then reflows. Rejects, sending
   * nothing, for WOFF or WOFF2, for a file over `MAX_FONT_BYTES`, or
   * when the transport fails; a file that holds no face closes the
   * session. */
  registerFont(data: ArrayBuffer | ArrayBufferView, family?: string): Promise<void> {
    // Copied now: the font may go after earlier acks, and the caller may
    // reuse, change or transfer its buffer meanwhile.
    const view = data instanceof ArrayBuffer
      ? new Uint8Array(data.slice(0))
      : new Uint8Array(data.buffer, data.byteOffset, data.byteLength).slice()
    const magic = String.fromCharCode(...view.subarray(0, 4))
    if (magic === "wOFF" || magic === "wOF2") {
      return Promise.reject(new TypeError(
        "registerFont: WOFF and WOFF2 aren't supported; pass the TTF or OTF file"))
    }
    if (view.byteLength > MAX_FONT_BYTES) {
      return Promise.reject(new RangeError(
        `registerFont: the file is ${view.byteLength} bytes; at most ${MAX_FONT_BYTES} fit a transaction`))
    }
    const sent = this.fontChain.then(async () => {
      await this.flush()
      this.encoder.font(family ?? null, view)
      await this.flush()
    })
    this.fontChain = sent.catch(() => {})
    return sent
  }

  /** Writes plain text to the system clipboard. */
  writeClipboard(text: string) {
    if (this.ready()) this.encoder.cmdWriteClipboard(NIL, text)
  }

  /** Text roots whose paragraph (own props or virtual descendants)
   * changed in this commit. */
  private dirtyTexts = new Set<HostNode>()

  private textRoot(n: HostNode): HostNode {
    while (n.textParent) n = n.textParent
    return n
  }

  private markText(n: HostNode) {
    if (this.ready()) this.dirtyTexts.add(this.textRoot(n))
  }

  /** Sends each dirty root's composed paragraph and listener mask, when
   * they differ from what it last sent. */
  private flushTexts() {
    for (const r of this.dirtyTexts) {
      if (r.mounted && !r.textParent) this.emitComposite(r)
    }
    this.dirtyTexts.clear()
  }

  /** A root text node's paragraph: its own text, then each nested Text
   * in order, each piece a span with its inherited style and its owner.
   * Hidden nested Text drops out. The native listener mask is the union
   * of the root's and its nested Texts' listeners. */
  private emitComposite(r: HostNode) {
    let text = ""
    let bytes = 0
    const spans: TextSpanIn[] = []
    const owners: HostNode[] = []
    // Each span's innermost pressable nested Text (what `spanTarget`
    // finds): consecutive spans of one join, so a link in two spans is
    // one press target natively.
    const pressers: (HostNode | undefined)[] = []
    let mask = 0
    const walk = (n: HostNode, style: TextSpanIn, presser: HostNode | undefined) => {
      mask |= listenerMask(n.props)
      // A hidden root is `display: none` natively and keeps its text, so
      // revealing it needs no recomposition; hidden nested Text drops out.
      if (n !== r && (n.suspended || n.props.hidden)) return
      if (n !== r && hasVariantKeys(n.props)) warnOnce("a nested Text takes no variants: put them on the outer Text")
      const own = textOf(n.props)
      if (own) {
        // A piece that opens with the low half of a pair the text before
        // ends with: the pair is one 4-byte character (3 bytes were
        // counted for the lone high half), and belongs to the span
        // before, so this span starts after it.
        const joins = isHigh(text.charCodeAt(text.length - 1)) && isLow(own.charCodeAt(0))
        const start = bytes + (joins ? 1 : 0)
        const last = spans.at(-1)
        if (!(last && owners.at(-1) === n && sameSpan(last, style))) {
          if (last && last.start === start) {
            // The span before is empty (all of it joined its pair).
            spans.pop()
            owners.pop()
            pressers.pop()
          }
          const sameLink = !!presser && pressers.at(-1) === presser
          spans.push(sameLink ? { ...style, start, pressJoins: true } : { ...style, start })
          owners.push(n)
          pressers.push(presser)
        }
        text += own
        bytes += utf8Length(own) - (joins ? 2 : 0)
      }
      for (const k of n.textKids ?? []) {
        walk(k, inheritSpan(style, k.props), isPressable(k.props) ? k : presser)
      }
    }
    const base = spanStyle(r.props)
    walk(r, base, undefined)
    if (spans.length === 0 || spans[0]!.start !== 0) {
      spans.unshift({ ...base, start: 0 })
      owners.unshift(r)
    }
    // New owners resend the paragraph even when it is unchanged: the
    // revision moves, so native events hit-tested against the old span
    // table do not reach the new owners.
    const prev = r.spanOwners
    const ownersChanged = !prev || prev.length !== owners.length ||
      owners.some((o, i) => o !== prev[i])
    r.spanOwners = owners
    const key = JSON.stringify([text, spans])
    if (key !== r.sentParagraph || ownersChanged) {
      r.sentParagraph = key
      r.paragraphRev = ((r.paragraphRev ?? 0) + 1) >>> 0
      this.encoder.paragraph(r.id, text, spans)
    }
    const lines = linesOf(r.props.numberOfLines)
    if (lines !== (r.sentLines ?? 0)) {
      r.sentLines = lines
      this.encoder.lines(r.id, lines)
    }
    const flags = interactionFlags(r.props)
    const interaction = `${mask},${flags}`
    if (interaction !== r.sentInteraction) {
      r.sentInteraction = interaction
      this.encoder.interaction(r.id, mask, flags)
    }
  }

  /** Nodes whose variants may have changed in this commit. */
  private dirtyVariants = new Set<HostNode>()

  /** Sends each dirty node's variant table when its signature changed.
   * Scopes resolve to ids here, once every node of the commit has one;
   * a variant of a scope that is gone drops out. */
  private flushVariants() {
    for (const n of this.dirtyVariants) {
      if (!n.mounted || n.textParent) continue
      const list: VariantIn[] = []
      const hidden = !!(n.props.hidden || n.suspended)
      for (const p of flattenVariants(n.props, n.props.__scopes ?? null)) {
        const values = variantValues(n, p.block, hidden)
        const motion = variantMotion(n, p.block, this.reducedMotion)
        if (!values && !motion.transitions && !motion.animations) continue
        const terms = []
        for (const [ref, mask] of p.terms) {
          if (!ref.node?.mounted) break
          terms.push({ scope: ref.node.id, mask })
        }
        if (terms.length !== p.terms.size) continue
        // The path's number keys the variant's animations: other blocks
        // appearing, going falsy or unsent leave them running.
        let block: number | undefined
        if (motion.animations) {
          const blocks = (n.variantBlocks ??= new Map())
          block = blocks.get(p.path)
          if (block === undefined) blocks.set(p.path, (block = blocks.size))
        }
        list.push({ terms, env: p.env, values: values ?? {}, ...motion, block })
      }
      const key = variantsKey(list)
      if (key !== (n.sentVariants ?? "")) {
        n.sentVariants = key
        this.encoder.variants(n.id, list)
      }
    }
    this.dirtyVariants.clear()
  }

  /** Sends pending ops and resolves once native acks the transaction. */
  flush(): Promise<void> {
    this.seal()
    const seq = this.seq
    if (!this.transport.onAck) return Promise.resolve()
    return new Promise((resolve) => this.flushWaiters.set(seq, resolve))
  }

  node(type: string, props: Record<string, any>): HostNode {
    const kind = KIND[type]
    if (kind === undefined) throw Error(`unknown element "${type}"`)
    const n: HostNode = {
      id: 0, gen: 0, type, kind, props, mounted: false, root: this,
      suspended: false, initial: [],
      focus() { this.root.cmd(this, (e, id) => e.cmdFocus(id)) },
      blur() { this.root.cmd(this, (e, id) => e.cmdBlur(id)) },
      scrollTo(x: number, y: number) {
        this.root.cmd(this, (e, id) => e.cmdScrollTo(id, x, y))
      },
      setText(text: string) {
        this.root.cmd(this, (e, id) => e.cmdSetText(id, text))
      },
      animate(prop: AnimProp, to: unknown, timing: Timing) {
        // Encode first: a bad call rejects with no op written and nothing
        // pending.
        let resolve!: (end: AnimationEnd) => void
        const done = new Promise<AnimationEnd>(r => { resolve = r })
        try {
          if (!(prop in ANIM_PROP)) throw Error(`unknown animated property "${String(prop)}"`)
          const value = animValue(prop, to)
          if (!this.mounted) return Promise.resolve({ finished: false, reason: "cancelled" as const })
          this.root.cmd(this, (e, id) => e.animate(id, prop, value, timing))
        } catch (err) {
          return Promise.reject(err)
        }
        ;(this.pendingAnims ??= []).push({ prop: ANIM_PROP[prop], resolve })
        return done
      },
      measure() {
        if (!this.mounted) return Promise.resolve(null)
        const [id, done] = this.root.request((ev): LayoutRect | null =>
          ev.revision ? { x: ev.x, y: ev.y, width: ev.a, height: ev.b } : null)
        this.root.cmd(this, (e, node) => e.cmdMeasure(node, id))
        return done
      },
    }
    return n
  }

  /** Emits an imperative command op in the current transaction. */
  cmd(n: HostNode, write: (enc: Encoder, id: number) => void) {
    if (!n.mounted) return
    if (this.ready()) write(this.encoder, n.id)
  }

  /** Routes a native event record to the target node's listener props.
   * Events for a previous occupant of the id are dropped. */
  private dispatchEvent(ev: UiEvent) {
    if (ev.kind === EVENT_KIND.environment) {
      this.setReducedMotion((ev.key & ENV_BIT.reducedMotion) !== 0)
      return
    }
    if (ev.kind === EVENT_KIND.claim) {
      this.dispatchClaim(ev)
      return
    }
    if (ev.kind === EVENT_KIND.frameStats) {
      const stats = {
        fps: ev.x,
        cpuMs: ev.y,
        maxCpuMs: ev.a,
        prepareMs: ev.b,
        nodes: ev.key,
        tweens: ev.revision,
      }
      for (const listener of frameStatsListeners) listener(stats)
      return
    }
    if (ev.kind === EVENT_KIND.exitEnd) {
      this.exitEnded(ev.node, ev.generation)
      return
    }
    // Answers go by request, whatever became of the node.
    if (ev.kind === EVENT_KIND.measure || ev.kind === EVENT_KIND.presented) {
      this.requests.get(ev.key)?.(ev)
      this.requests.delete(ev.key)
      return
    }
    if (ev.kind === EVENT_KIND.window) {
      const w = {
        width: ev.x,
        height: ev.y,
        scale: ev.a,
        focused: (ev.key & WINDOW_BIT.focused) !== 0,
        visible: (ev.key & WINDOW_BIT.visible) !== 0,
        dark: (ev.key & WINDOW_BIT.dark) !== 0,
      }
      this.windowState = w
      for (const listener of this.windowListeners) listener(w)
      return
    }
    const root = this.nodes.get(ev.node)
    if (!root || root.gen !== ev.generation) return
    // A pointer event on a text root carries the span under the pointer
    // (key bits 16+, 0: none): it goes to the innermost nested Text of
    // that span with a listener for it, else to the root.
    const n = this.pressTarget(root, ev)
    const p = n.props
    const e = { target: n, x: ev.x, y: ev.y }
    // Pointer events pack mods into the low 4 key bits and the button
    // into bits 8+, leaving a/b for hit-relative coordinates.
    const pointer = {
      ...e,
      rx: ev.a,
      ry: ev.b,
      button: (ev.key >>> 8) & 0xff,
      shift: !!(ev.key & 1),
      ctrl: !!(ev.key & 2),
      alt: !!(ev.key & 4),
      meta: !!(ev.key & 8),
    }
    switch (ev.kind) {
      case EVENT_KIND.pointerMove: p.onPointerMove?.(pointer); break
      case EVENT_KIND.pointerDown: p.onPointerDown?.(pointer); break
      case EVENT_KIND.pointerUp: p.onPointerUp?.(pointer); break
      case EVENT_KIND.press: {
        const phase = (ev.key >>> 4) & 3
        if (phase === PRESS_PHASE.in) p.onPressIn?.(pointer)
        else p.onPressOut?.({ ...pointer, cancelled: phase === PRESS_PHASE.cancel })
        break
      }
      case EVENT_KIND.activate:
        p.onPress?.({ ...pointer, source: ACTIVATE_SOURCE[(ev.key >>> 4) & 3] ?? "pointer" })
        break
      case EVENT_KIND.pointerEnter: p.onPointerEnter?.(e); break
      case EVENT_KIND.pointerLeave: p.onPointerLeave?.(e); break
      case EVENT_KIND.wheel: p.onWheel?.({ ...e, dx: ev.a, dy: ev.b }); break
      case EVENT_KIND.keyDown: p.onKeyDown?.(keyEvt(e, ev)); break
      case EVENT_KIND.keyUp: p.onKeyUp?.(keyEvt(e, ev)); break
      case EVENT_KIND.focus: p.onFocus?.(e); break
      case EVENT_KIND.blur: p.onBlur?.(e); break
      case EVENT_KIND.change: p.onChangeText?.(ev.text); break
      case EVENT_KIND.submit: p.onSubmit?.(ev.text); break
      case EVENT_KIND.scroll: p.onScroll?.({ target: n, x: ev.a, y: ev.b }); break
      case EVENT_KIND.layout: p.onLayout?.({ target: n, x: ev.x, y: ev.y, width: ev.a, height: ev.b }); break
      case EVENT_KIND.animationEnd: {
        const prop = ev.key & 0xff
        const reason = END_REASON[(ev.key >>> 8) & 0xff] ?? "cancelled"
        const trigger = (ev.key >>> 16) & 0xff
        if (trigger) {
          // A keyframe animation's (key: its index in the prop | reason
          // << 8 | trigger + 1 << 16).
          const index = prop
          const animation = trigger === 1 ? "enter" : "animation"
          root.props.onAnimationEnd?.({ target: root, animation, index, finished: reason === "finished", reason })
          break
        }
        const list = root.pendingAnims ?? []
        const i = list.findIndex(p => p.prop === prop)
        if (i >= 0) list.splice(i, 1)[0]!.resolve({ finished: reason === "finished", reason })
        break
      }
      case EVENT_KIND.image:
        if (ev.key === 0) p.onLoad?.({ target: n, width: ev.x, height: ev.y })
        else p.onError?.({ target: n, message: ev.text })
        break
      case EVENT_KIND.listRange: {
        // The kept (focused) item by identity: its index may be stale by
        // the time this arrives. Indices apply only to the item order
        // native saw, the current revision; native reports again after
        // every splice.
        const keys = n.list
        const keepKey = ev.key === NIL || !keys ? undefined : keys.keys.get(ev.key)
        const current = !keys || ev.y === (keys.revision & 0xff_ffff)
        p.onRange?.({ first: ev.a, end: ev.b, keepKey, current })
        break
      }
    }
  }

  /** A claim matched: runs the handler of the version native matched,
   * and sends its answer (text to insert, text for the clipboard). */
  private dispatchClaim(ev: UiEvent) {
    const n = ev.node === NIL ? undefined : this.nodes.get(ev.node)
    if (ev.node !== NIL && (!n || n.gen !== ev.generation)) return
    const state = ev.node === NIL ? this.windowClaims : n?.claims
    const run = state?.handlers.get(ev.revision)?.[ev.key >>> 8]
    if (!run) return
    const kind = ev.key & 0xff
    if (kind === CLAIM_KIND.key) {
      run()
      return
    }
    if (!n) return
    const e = { target: n, x: ev.x, y: ev.y }
    switch (kind) {
      case CLAIM_KIND.paste: {
        const out = run({ target: n, text: ev.text })
        if (typeof out === "string") this.cmd(n, (enc, id) => enc.cmdInsertText(id, out))
        break
      }
      case CLAIM_KIND.copy: {
        const out = run({ target: n, text: ev.text })
        if (typeof out === "string") this.writeClipboard(out)
        break
      }
      case CLAIM_KIND.cut: {
        const out = run({ target: n, text: ev.text })
        if (typeof out === "string") {
          this.writeClipboard(out)
          this.cmd(n, (enc, id) => enc.cmdInsertText(id, ""))
        }
        break
      }
      case CLAIM_KIND.drop: run({ ...e, paths: ev.text ? ev.text.split("\0") : [] }); break
      case CLAIM_KIND.contextMenu: run(e); break
    }
  }

  /** `spanTarget`, except a press's out or cancel goes to whoever heard
   * its start, while it is still in the text: native ends a press with
   * the span table it began in (a re-render mid-press cancels it). */
  private pressTarget(root: HostNode, ev: UiEvent): HostNode {
    if (ev.kind !== EVENT_KIND.press) return this.spanTarget(root, ev)
    if (((ev.key >>> 4) & 3) === PRESS_PHASE.in) {
      const n = this.spanTarget(root, ev)
      root.pressOwner = n
      return n
    }
    const owner = root.pressOwner
    root.pressOwner = undefined
    for (let m = owner; m; m = m.textParent) if (m === root) return owner!
    return this.spanTarget(root, ev)
  }

  private spanTarget(root: HostNode, ev: UiEvent): HostNode {
    const span = ev.key >>> 16
    const press = ev.kind === EVENT_KIND.press || ev.kind === EVENT_KIND.activate
    const handler = POINTER_HANDLER[ev.kind]
    if (!root.spanOwners || span === 0 || !(handler || press)) return root
    // Hit-tested against another span table: the span index is stale.
    if (ev.revision !== (root.paragraphRev ?? 0)) return root
    // Presses go to the innermost pressable nested Text (the one native
    // marked the span for), pointer events to the innermost listener.
    for (let n: HostNode | undefined = root.spanOwners[span - 1]; n; n = n.textParent) {
      if (n === root) break
      if (press ? isPressable(n.props) : typeof n.props[handler!] === "function") return n
    }
    return root
  }

  /** Emits create + props + placement for a subtree root. Children that
   * arrived before mount are replayed by the reconciler's place calls. */
  materialize(n: HostNode, parent: HostNode | null, before: HostNode | null) {
    if (n.mounted) return
    n.id = this.alloc()
    n.gen = this.gens[n.id]!
    n.mounted = true
    // A fresh native node holds nothing yet: forget what an earlier
    // native node of this text sent (it may have been nested since).
    n.sentParagraph = undefined
    n.sentInteraction = undefined
    n.spanOwners = undefined
    n.pressOwner = undefined
    n.paragraphRev = 0
    n.claims = undefined
    n.sentBits = undefined
    n.sentVariants = undefined
    n.variantBlocks = undefined
    n.sentTransitions = undefined
    n.sentAnimation = undefined
    n.exit = undefined
    n.parent = parent
    this.nodes.set(n.id, n)
    if (!this.ready()) return
    const enc = this.encoder
    enc.create(n.id, n.kind)
    this.emitProps(n, {}, n.props, false)
    enc.place(parent ? parent.id : NIL, n.id, before ? before.id : NIL)
    for (const child of n.initial) this.place(n, child, null)
    n.initial = []
  }

  /** A layer container: a view filling the window under the root, above
   * the layer it was opened from (`outer`), owned natively by `owner`'s
   * node (a FocusTrap's, else `outer`). Opened on its first child. */
  layer(outer: HostNode | null, owner: OwnerRef | null, z: number): HostNode {
    const n = this.node("view", { style: { width: "100%", height: "100%", zIndex: z } })
    n.layer = { outer, owner, kids: new Set(), owned: new Set() }
    return n
  }

  /** Places `child` in a layer container, opening the layer (and its
   * owner, first) at the top of the root level if needed. */
  placeInLayer(layer: HostNode, child: HostNode, before: HostNode | null) {
    this.openLayer(layer)
    layer.layer!.kids.add(child)
    this.place(layer, child, before)
  }

  /** Removes `child` from a layer container; the last one out closes
   * the layer (it opens again, on top, with its next child). */
  removeFromLayer(layer: HostNode, child: HostNode) {
    this.detach(child)
    // An exiting child keeps the layer open until its exit ends.
    if (child.exit) {
      child.exit.layer = layer
      return
    }
    layer.layer!.kids.delete(child)
    this.closeIdle(layer)
  }

  private openLayer(n: HostNode) {
    if (n.mounted || !n.layer) return
    const outer = n.layer.outer
    if (outer) {
      this.openLayer(outer)
      outer.layer!.owned.add(n)
    }
    this.materialize(n, null, null)
    this.layers.push(n)
    // The owner's id at the seal: a portal's children commit before its
    // ancestors, so a FocusTrap around it may have none yet.
    if (this.ready()) this.dirtyLayers.push(n)
  }

  /** Layers opened in this commit, in open order. */
  private dirtyLayers: HostNode[] = []

  /** Sends each layer opened in this commit with its owner. */
  private flushLayers() {
    for (const n of this.dirtyLayers) {
      if (!n.mounted) continue
      const o = n.layer!.owner?.node
      this.encoder.layer(n.id, o?.mounted ? o.id : NIL)
    }
    this.dirtyLayers = []
  }

  /** Closes a layer with neither children nor open layers it owns, and
   * then its owner if that leaves it idle too. An owner stays open while
   * a layer it owns is: native would drop the owner on its removal. */
  private closeIdle(n: HostNode) {
    const l = n.layer!
    if (!n.mounted || l.kids.size > 0 || l.owned.size > 0) return
    this.release(n)
    if (!l.outer) return
    l.outer.layer!.owned.delete(n)
    this.closeIdle(l.outer)
  }

  place(parent: HostNode | null, child: HostNode, before: HostNode | null) {
    // The app's root nodes stay below the layers, which React may have
    // opened first (a portal's children commit before its ancestors).
    if (!parent && !before) before = this.layers[0] ?? null
    if (parent && parent.type === "text" && child.type === "text") {
      this.placeVirtual(parent, child, before)
      return
    }
    // A nested text moving out of text becomes a native root: leave the
    // old parent's paragraph first.
    if (child.textParent) this.unlinkVirtual(child)
    if (!child.mounted) {
      this.materialize(child, parent, before)
      return
    }
    child.parent = parent
    if (this.ready()) {
      this.encoder.place(parent ? parent.id : NIL, child.id, before ? before.id : NIL)
    }
  }

  /** A text node inside a text node: no native node; it joins the
   * parent's list of nested text and the root's paragraph. A node that
   * had a native node gives it up. Children that arrived before it was
   * placed join it too. */
  private placeVirtual(parent: HostNode, child: HostNode, before: HostNode | null) {
    if (child.mounted && !child.textParent) this.release(child)
    this.unlinkVirtual(child)
    const kids = (parent.textKids ??= [])
    const at = before ? kids.indexOf(before) : -1
    if (at >= 0) kids.splice(at, 0, child)
    else kids.push(child)
    child.textParent = parent
    for (const c of child.initial) this.place(child, c, null)
    child.initial = []
    this.markText(parent)
  }

  /** Removes a virtual text node from its parent's nested list. */
  private unlinkVirtual(n: HostNode) {
    const p = n.textParent
    if (!p) return
    this.markText(p)
    const kids = p.textKids ?? []
    const i = kids.indexOf(n)
    if (i >= 0) kids.splice(i, 1)
    n.textParent = undefined
  }

  /** Unlinks `n` without freeing its slot — removal unmounts it; the
   * node's own `release` (via detachDeletedInstance) frees it. With an
   * exit sent, native runs it and frees the subtree at its end. */
  detach(n: HostNode) {
    if (n.textParent) {
      this.unlinkVirtual(n)
      return
    }
    if (!n.mounted || !this.ready()) return
    // The exit goes with the detach, under the policy of the moment.
    const exit = n.props.exit && !this.unmounting
      ? animationList(n.props.exit, FILL.forwards, this.reducedMotion, n.kind !== 1, true)
      : []
    if (exit.length) {
      this.encoder.animation(n.id, ANIMATION_TRIGGER.exit, false, exit)
      n.exit = { node: n, gen: n.gen, parked: [], ended: false }
      this.exits.set(n.id, n.exit)
    } else {
      n.parent = null
    }
    this.encoder.detach(n.id)
  }

  /** The exit `n` is part of (its own or an ancestor's), if any. */
  private exitOf(n: HostNode): Exit | undefined {
    for (let m: HostNode | null | undefined = n; m; m = m.parent) if (m.exit) return m.exit
    return undefined
  }

  /** Recycles an id native has freed. */
  private recycle(id: number) {
    this.gens[id] = (this.gens[id]! + 1) & 0xffff
    this.freeIds.push(id)
  }

  /** Native ended the exit of `id` and freed its subtree: the ids
   * released so far recycle, the rest as React releases them. */
  private exitEnded(id: number, gen: number) {
    const exit = this.exits.get(id)
    if (!exit || exit.gen !== gen) return
    this.exits.delete(id)
    exit.ended = true
    for (const p of exit.parked) this.recycle(p)
    exit.parked = []
    const layer = exit.layer
    if (layer) {
      layer.layer!.kids.delete(exit.node)
      this.closeIdle(layer)
    }
  }

  /** Unmounts the app: `clear` renders nothing, its removals starting
   * no exit, then every running exit ends at once. */
  unmount(clear: () => void) {
    this.unmounting = true
    try {
      clear()
    } finally {
      this.unmounting = false
    }
    this.endExits()
  }

  /** Ends every running exit at once: native frees their subtrees now
   * (reason `removed`), and says so. An exit native has just finished
   * (its end not seen yet) is no longer there: `endExit`, unlike
   * `remove`, then does nothing. Each exit is cut once. */
  private endExits() {
    const cut = [...this.exits.values()].filter(e => !e.cut)
    for (const exit of cut) {
      exit.cut = true
      if (this.ready()) this.encoder.endExit(exit.node.id)
      exit.layer?.layer!.kids.delete(exit.node)
    }
    for (const exit of cut) {
      const layer = exit.layer
      exit.layer = undefined
      if (layer) this.closeIdle(layer)
    }
  }

  /** React deleted `n` for good: free the native slot and recycle the
   * id at once. The generation bump keeps stale events out. */
  release(n: HostNode) {
    if (n.textParent) {
      this.unlinkVirtual(n)
      return
    }
    if (!n.mounted) return
    this.nodes.delete(n.id)
    const at = n.layer ? this.layers.indexOf(n) : -1
    if (at >= 0) this.layers.splice(at, 1)
    n.claims = undefined
    // Its native end events will not reach it (the generation moves).
    for (const p of n.pendingAnims?.splice(0) ?? []) p.resolve({ finished: false, reason: "removed" })
    n.mounted = false
    // Inside an exit: native frees it with the subtree.
    const exit = this.exitOf(n)
    if (exit && !exit.ended) exit.parked.push(n.id)
    else if (exit) this.recycle(n.id)
    else {
      if (this.ready()) this.encoder.remove(n.id)
      this.recycle(n.id)
    }
  }

  /** React (Suspense) hides or reveals a node: `display: none`. */
  setSuspended(n: HostNode, hidden: boolean) {
    if (n.suspended === hidden) return
    if (n.textParent) {
      n.suspended = hidden
      this.markText(n)
      return
    }
    const before = styleKey(layoutOf(n.props, n.suspended))
    n.suspended = hidden
    const layout = layoutOf(n.props, n.suspended)
    if (styleKey(layout) !== before && this.ready()) this.encoder.layout(n.id, layout)
    // Its variants' `display` stops (or resumes) applying.
    if (n.sentVariants || hasVariantKeys(n.props)) this.dirtyVariants.add(n)
  }

  /** Prop diff -> ops for the fields that changed. */
  update(n: HostNode, oldProps: Record<string, any>, props: Record<string, any>) {
    n.props = props
    if (n.textParent) {
      this.markText(n)
      return
    }
    if (this.ready()) this.emitProps(n, oldProps, props, true)
  }

  setTextContent(n: HostNode, text: string) {
    const old = n.props
    n.props = { ...n.props, text, children: undefined }
    if (n.textParent) {
      this.markText(n)
      return
    }
    if (this.ready()) this.emitProps(n, old, n.props, true)
  }

  /** Writes the ops for props that differ between old and next. `mounted`
   * is false on first emission, when native holds only defaults. */
  private emitProps(
    n: HostNode,
    oldProps: Record<string, any>,
    props: Record<string, any>,
    mounted: boolean,
  ) {
    const enc = this.encoder
    const id = n.id

    // Transitions: a change applies to this commit's changes (CSS uses
    // the after-change style), so it goes first; at mount it goes last,
    // so first values do not tween.
    if (mounted) this.sendTransitions(n)

    // Layout inputs (spatial keys split off; hiding is display: none).
    const oldLayout = mounted ? layoutOf(oldProps, n.suspended) : undefined
    const newLayout = layoutOf(props, n.suspended)
    if (styleKey(oldLayout) !== styleKey(newLayout)) enc.layout(id, newLayout)

    // Spatial: the transform parts, opacity and z never touch layout.
    // Only what changed goes: a rotate change leaves a running scale
    // tween alone.
    const oldP = partsOf(oldProps.style), newP = partsOf(props.style)
    const oldO = oldProps.style?.opacity ?? 1
    const newO = props.style?.opacity ?? 1
    const oldZ = zOf(oldProps.style), newZ = zOf(props.style)
    const sp = {
      transform: same(oldP.matrix, newP.matrix) ? undefined : newP.matrix,
      translate: same(oldP.translate, newP.translate) ? undefined : newP.translate,
      rotate: oldP.rotate === newP.rotate ? undefined : newP.rotate,
      scale: same(oldP.scale, newP.scale) ? undefined : newP.scale,
      opacity: oldO === newO ? undefined : newO,
      z: oldZ === newZ ? undefined : newZ,
    }
    if (Object.values(sp).some(v => v !== undefined)) enc.spatial(id, sp)

    if (n.kind === 1) {
      // Paragraph and listeners: composed with nested Text at the seal.
      this.markText(n)
    } else {
      // Box paint: fill, corner radius, border — each an optional masked
      // field; emit only what changed.
      const oldBg = color(oldProps.backgroundColor), newBg = color(props.backgroundColor)
      const oldR = oldProps.borderRadius ?? 0, newR = props.borderRadius ?? 0
      const oldBc = color(oldProps.borderColor), newBc = color(props.borderColor)
      const oldBw = oldProps.borderWidth ?? 0, newBw = props.borderWidth ?? 0
      const sides = sidesOf(props), oldSides = sidesOf(oldProps)
      const newSides = sidesKey(sides) !== sidesKey(oldSides)
        ? sides ?? { widths: [0, 0, 0, 0], colors: [0, 0, 0, 0] } as BorderSidesIn
        : undefined
      const shadows = oldProps.boxShadow === props.boxShadow ? undefined : shadowList(props.boxShadow)
      const newShadows = shadows && shadowKey(shadows) !== shadowKey(shadowList(oldProps.boxShadow))
        ? shadows
        : undefined
      if (oldBg !== newBg || oldR !== newR || oldBc !== newBc || oldBw !== newBw || newShadows || newSides) {
        enc.paint(
          id,
          oldBg !== newBg ? newBg : undefined,
          oldR !== newR ? newR : undefined,
          oldBc !== newBc || oldBw !== newBw ? { color: newBc, width: newBw } : undefined,
          newShadows,
          newSides,
        )
      }
    }

    // The inherited color: spans, inputs and a drawing's currentColor
    // paint with the nearest one.
    if (n.kind !== 3) {
      const oldC = mounted && oldProps.color !== undefined ? color(oldProps.color) : null
      const newC = props.color !== undefined ? color(props.color) : null
      if (oldC !== newC) enc.color(id, newC)
    }

    // A scope: its app state bits. It is one from its first STATES on.
    const scope: ScopeRef | undefined = props.__scope
    if (scope) {
      scope.node = n
      const bits = stateBits(props)
      if (bits !== n.sentBits) {
        n.sentBits = bits
        enc.states(id, bits)
      }
    } else if (APP_STATES.some(k => props[k] !== undefined) || props.states !== undefined) {
      warnOnce("state props need a scope: a Pressable or a View with group")
    }
    // Variants resolve at the seal, where every scope has an id.
    if (n.sentVariants || hasVariantKeys(props)) this.dirtyVariants.add(n)

    if (!mounted) this.sendTransitions(n)

    if (n.kind === 3) {
      // Surface kind + params; the payload is a typed array copied once
      // per change (identity compare: a new array means new data).
      const oldParams = surfaceParams(oldProps.params)
      const newParams = surfaceParams(props.params)
      const kind = props.kind ?? 0
      if (
        !mounted || kind !== (oldProps.kind ?? 0) ||
        newParams.some((v, i) => v !== oldParams[i])
      ) {
        enc.surface(id, kind, newParams)
      }
      if (props.payload !== undefined && props.payload !== oldProps.payload) {
        enc.payload(id, props.payload)
      }
    }

    if (n.kind === 5 && !mounted) n.sentDrawing = undefined
    if (n.kind === 5 && props.asset !== undefined && props.asset !== oldProps.asset) {
      // Vector: the asset bytes (`craie-svg` output), copied once per
      // change (identity compare).
      enc.payload(id, props.asset)
      n.sentDrawing = undefined
    }
    if (n.kind === 5 && props.shapes !== undefined) {
      // Vector: runtime shapes, flattened by `Vector` on every render;
      // sent when their content changes (one stringify a render, compared
      // with the last one sent). Native interns by content too, so equal
      // drawings parse and tessellate once.
      const drawing = JSON.stringify([props.viewBox, props.shapes])
      if (drawing !== n.sentDrawing) {
        enc.drawing(id, props.viewBox, props.shapes)
        n.sentDrawing = drawing
      }
    } else if (n.kind === 5 && props.asset === undefined && n.sentDrawing !== "") {
      // Neither: an empty drawing (an empty view box draws nothing).
      if (mounted) enc.drawing(id, "", [])
      n.sentDrawing = ""
    }

    if (n.kind === 7) {
      // Image: the encoded bytes, copied once per change (identity
      // compare; `Image` keeps the old ones until new ones arrive), and
      // the fit (native's default is cover).
      if (props.bytes !== undefined && props.bytes !== oldProps.bytes) {
        enc.payload(id, props.bytes)
      }
      const fit = FIT[props.fit as ImageFit] ?? FIT.cover
      const oldFit = mounted ? FIT[oldProps.fit as ImageFit] ?? FIT.cover : FIT.cover
      if (fit !== oldFit) enc.imageConfig(id, fit)
    }

    if (n.kind === 4) {
      // List: configuration, then the item diff as one splice (common
      // prefix and suffix by identity, as React compares props).
      const templates: readonly ListTemplate[] = props.templates ?? []
      const overscan = props.overscan ?? 400
      const fallback = props.estimatedItemSize ?? 44
      if (
        !mounted ||
        overscan !== (oldProps.overscan ?? 400) ||
        fallback !== (oldProps.estimatedItemSize ?? 44) ||
        JSON.stringify(templates) !== JSON.stringify(oldProps.templates ?? [])
      ) {
        enc.listConfig(id, overscan, fallback, templates)
      }
      const before: readonly unknown[] = mounted ? oldProps.items ?? [] : []
      const items: readonly unknown[] = props.items ?? []
      if (before !== items) {
        let pre = 0
        while (pre < before.length && pre < items.length && before[pre] === items[pre]) pre++
        let suf = 0
        while (
          suf < before.length - pre && suf < items.length - pre &&
          before[before.length - 1 - suf] === items[items.length - 1 - suf]
        ) suf++
        const remove = before.length - pre - suf
        const added = items.slice(pre, items.length - suf)
        if (remove > 0 || added.length > 0) {
          // Identity: each item's key interned to an id, so an item that
          // moves within the splice keeps its anchor, measurement, and
          // focused row natively.
          const keys = (n.list ??= { ids: new Map(), keys: new Map(), next: 0, revision: 0 })
          const keyOf: (item: any, index: number) => unknown = props.keyOf ?? ((_: any, i: number) => i)
          const describe: (item: any) => ItemDesc = props.describe ?? (() => ({}))
          const intern = (key: unknown) => {
            let v = keys.ids.get(key)
            if (v === undefined) {
              v = keys.next++
              keys.ids.set(key, v)
              keys.keys.set(v, key)
            }
            return v
          }
          // Removed items by key: an added item that is the same object
          // moved (items are immutable) keeps its measured height; a new
          // object under the same key is an edit and is estimated again.
          const removedByKey = new Map<unknown, unknown>()
          for (let i = pre; i < before.length - suf; i++) removedByKey.set(keyOf(before[i], i), before[i])
          const addedKeys = new Set<unknown>()
          const descs = added.map((item, k) => {
            const key = keyOf(item, pre + k)
            addedKeys.add(key)
            return { ...describe(item), id: intern(key), unchanged: removedByKey.get(key) === item }
          })
          for (let i = pre; i < before.length - suf; i++) {
            const key = keyOf(before[i], i)
            if (addedKeys.has(key)) continue
            const v = keys.ids.get(key)
            if (v !== undefined) {
              keys.ids.delete(key)
              keys.keys.delete(v)
            }
          }
          enc.listSplice(id, pre, remove, descs)
          keys.revision++
        }
      }
    }

    // A list row's item index.
    if ((oldProps.listIndex ?? NIL) !== (props.listIndex ?? NIL)) {
      enc.listIndex(id, props.listIndex ?? NIL)
    }
    // A scroll container's anchoring policy.
    const oldAnchor: ScrollAnchor = mounted ? oldProps.anchor ?? "keep-visible" : "keep-visible"
    const newAnchor: ScrollAnchor = props.anchor ?? "keep-visible"
    if (oldAnchor !== newAnchor) enc.scrollAnchor(id, newAnchor)

    if (n.kind === 2) {
      // INPUT config. The input is uncontrolled (ARCHITECTURE.md §5):
      // `value` is the initial text, sent once at mount; later changes
      // go through the `setText` command, so native edits are never
      // overwritten by a stale prop.
      const fs = props.fontSize ?? 14
      const ph = props.placeholder ?? ""
      const multiline = !!props.multiline
      // Enter submits only with `onSubmit`, as in the kit; without it a
      // multiline input takes Enter as a newline.
      const submit = submitKeyOf(props)
      if (
        !mounted ||
        oldProps.fontSize !== props.fontSize ||
        (oldProps.placeholder ?? "") !== ph ||
        !!oldProps.multiline !== multiline ||
        submitKeyOf(oldProps) !== submit
      ) {
        enc.inputConfig(id, fs, ph, multiline, submit)
      }
      if (!mounted && typeof props.value === "string" && props.value !== "") {
        enc.cmdSetText(id, props.value)
      }
    }

    // Listener mask, interaction and press flags (a text root's: at
    // the seal).
    const oldMask = listenerMask(oldProps), newMask = listenerMask(props)
    const newFlags = interactionFlags(props)
    if (n.kind !== 1 && (oldMask !== newMask || interactionFlags(oldProps) !== newFlags)) {
      enc.interaction(id, newMask, newFlags)
    }

    // A FocusTrap's node: it owns the layers opened inside it, and
    // carries the trap's flags.
    const owner: OwnerRef | undefined = props.__owner
    if (owner) owner.node = n
    if (props.__trap !== undefined && props.__trap !== oldProps.__trap) {
      enc.trap(id, props.__trap)
    }
    // A FocusGroup's node carries the group's flags.
    if (props.__group !== undefined && props.__group !== oldProps.__group) {
      enc.group(id, props.__group)
    }

    // Claims: a new version when the declaration changes; the handlers
    // follow every commit.
    if (n.claims || claimsDeclared(props)) {
      this.sendClaims(id, (n.claims ??= { sig: "", version: 0, handlers: new Map() }), declaredClaims(props))
    }

    const oldRole = mounted ? roleOf(oldProps) : ROLE.none
    const newRole = roleOf(props)
    const oldReported = mounted ? reportedOf(oldProps) : 0
    const newReported = reportedOf(props)
    if (oldRole !== newRole || oldReported !== newReported) enc.role(id, newRole, newReported)

    const oldLabel = oldProps.accessibilityLabel ?? ""
    const newLabel = props.accessibilityLabel ?? ""
    if (oldLabel !== newLabel) enc.label(id, newLabel)

    // Keyframe animations: `enter` with the creation (native runs it
    // only in the transaction that creates the node), `animation` when
    // its list changes (a new list restarts; the same one keeps
    // running).
    if (!mounted && props.enter) this.sendAnimations(n, 0)
    this.sendAnimations(n, 1)
    // `exit` goes with the removal (`detach`); it is checked here.
    if (props.exit && props.exit !== oldProps.exit) {
      animationList(props.exit, FILL.forwards, false, n.kind !== 1, true)
    }
  }

  /** Sends the node's transitions under the reduced-motion policy when
   * they differ from those last sent. */
  private sendTransitions(n: HostNode) {
    const t = transitionsIn(n.props.style?.transition, this.reducedMotion)
    const key = transitionKey(t)
    if (key === (n.sentTransitions ?? "")) return
    n.sentTransitions = key
    this.encoder.transition(n.id, t)
  }

  /** Sends `enter` (trigger 0), or `animation` (1) when it differs from
   * the list last sent. */
  private sendAnimations(n: HostNode, trigger: 0 | 1) {
    const p = n.props
    const fill = trigger === 0 ? FILL.backwards : FILL.none
    const list = animationList(trigger === 0 ? p.enter : p.animation, fill, this.reducedMotion, n.kind !== 1)
    const notify = p.onAnimationEnd !== undefined && list.some(a => a.iterations !== Infinity)
    if (trigger === 1) {
      const key = list.length ? JSON.stringify([notify, list]) : ""
      if (key === (n.sentAnimation ?? "")) return
      n.sentAnimation = key
    } else if (!list.length) {
      return
    }
    this.encoder.animation(n.id, trigger, notify, list)
  }

  /** The reduced-motion setting changed: each node's transitions,
   * animation list and variants go again under their policies. A
   * running `enter` keeps on. */
  private setReducedMotion(on: boolean) {
    if (on === this.reducedMotion) return
    this.reducedMotion = on
    for (const n of this.nodes.values()) {
      if (n.sentTransitions || n.props.style?.transition) this.sendTransitions(n)
      if (n.sentAnimation || n.props.animation) this.sendAnimations(n, 1)
      if (n.sentVariants || hasVariantKeys(n.props)) this.dirtyVariants.add(n)
    }
    this.ready()
  }
}

