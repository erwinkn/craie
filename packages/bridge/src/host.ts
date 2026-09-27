// Commit-side host bookkeeping: JS owns node ids, maps element props to
// CRW2 ops, and seals one transaction per commit via queueMicrotask.
//
// Ids recycle immediately on removal. Native bumps a slot's generation
// when it frees it; JS mirrors the counter, and events carry the
// generation, so an event for a previous occupant of an id is dropped.

import {
  DECORATION,
  Encoder,
  EVENT_KIND,
  EVENT_MASK,
  NIL,
  ROLE,
  layoutPart,
  styleKey,
  transformMatrix,
  type AccessibilityRole,
  type Affine,
  type AnimProp,
  type AnimationEnd,
  ANIM_PROP,
  END_REASON,
  type Timing,
  type Transitions,
  type ItemDesc,
  type ListTemplate,
  type ScrollAnchor,
  type StyleProps,
  type TextSpanIn,
} from "./wire.js"

// 0 view, 1 text, 2 input, 3 surface, 4 list — mirror NodeKind
export type Kind = 0 | 1 | 2 | 3 | 4 | 5
export const KIND: Record<string, Kind> = { view: 0, text: 1, input: 2, surface: 3, list: 4, vector: 5 }

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
  /** Key code (key events); mirror events.rs `Key::code`. */
  key: number
  /** Text payload (change/submit). */
  text: string
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
  /** Text nodes: nested text nodes, in order. They have no native node
   * (virtual); their text and style become spans of the root's
   * paragraph. */
  textKids?: HostNode[]
  /** A virtual text node's parent text node. */
  textParent?: HostNode
  /** A text root: the node that owns each span it last sent (event
   * routing), and what it last sent. */
  spanOwners?: HostNode[]
  sentParagraph?: string
  sentInteraction?: string
  /** Paragraph ops sent for this node, wrapping at 2^32 (mirrors native
   * `Paragraph::revision`): a span event from another revision was
   * hit-tested against an older span table. */
  paragraphRev?: number
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
   * transform list, colors as in props, padding a number or [left,
   * right, top, bottom], gap a number or [column, row]. Resolves when
   * the tween ends: finished, cancelled (a commit set the property),
   * retargeted (another tween replaced it), or removed (with its
   * node). */
  animate(prop: AnimProp, to: unknown, timing: Timing): Promise<AnimationEnd>
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

/** "#rgb" / "#rrggbb" / "#rrggbbaa" / number -> 0xRRGGBBAA. */
export function color(v: string | number | undefined, fallback = 0): number {
  if (v === undefined) return fallback
  if (typeof v === "number") return v >>> 0
  let s = v.startsWith("#") ? v.slice(1) : v
  if (s.length === 3) s = [...s].map(c => c + c).join("") + "ff"
  if (s.length === 6) s += "ff"
  if (s.length !== 8) throw Error(`bad color "${v}"`)
  const n = parseInt(s, 16)
  if (!Number.isFinite(n)) throw Error(`bad color "${v}"`)
  return n >>> 0
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
function utf8Length(s: string): number {
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
    case "backgroundColor": case "borderColor": return [color(to as string | number)]
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
    color: props.color !== undefined ? own.color : parent.color,
    weight: props.fontWeight !== undefined ? own.weight : parent.weight,
    italic: props.fontStyle !== undefined ? own.italic : parent.italic,
    fontFamily: props.fontFamily !== undefined ? own.fontFamily : parent.fontFamily,
    decoration: props.textDecorationLine !== undefined ? own.decoration : parent.decoration,
    letterSpacing: props.letterSpacing !== undefined ? own.letterSpacing : parent.letterSpacing,
    lineHeight: parent.lineHeight,
  }
}

/** Span zero of a text node: the base style. */
function decorationOf(line: unknown): number {
  if (typeof line !== "string") return 0
  return (line.includes("underline") ? DECORATION.underline : 0) |
    (line.includes("line-through") ? DECORATION.lineThrough : 0)
}

/** The span a Text's own props describe (its base style). */
export function spanStyle(props: Record<string, any>): TextSpanIn {
  return {
    start: 0,
    fontSize: props.fontSize ?? 14,
    color: color(props.color, 0xffff_ffff),
    weight: typeof props.fontWeight === "number"
      ? props.fontWeight
      : props.fontWeight === "bold" ? 700 : 400,
    italic: props.fontStyle === "italic",
    fontFamily: props.fontFamily,
    decoration: decorationOf(props.textDecorationLine),
    letterSpacing: props.letterSpacing ?? 0,
    lineHeight: props.lineHeight ?? 0,
  }
}

function sameSpan(a: TextSpanIn, b: TextSpanIn): boolean {
  return a.fontSize === b.fontSize && a.color === b.color &&
    a.weight === b.weight && !!a.italic === !!b.italic &&
    a.fontFamily === b.fontFamily && (a.decoration ?? 0) === (b.decoration ?? 0) &&
    (a.letterSpacing ?? 0) === (b.letterSpacing ?? 0) && (a.lineHeight ?? 0) === (b.lineHeight ?? 0)
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

function sameMatrix(a: Affine, b: Affine): boolean {
  for (let i = 0; i < 6; i++) if (a[i] !== b[i]) return false
  return true
}

function roleOf(props: Record<string, any>): number {
  const r = props.accessibilityRole as AccessibilityRole | undefined
  if (r === undefined) return ROLE.none
  const v = ROLE[r]
  if (v === undefined) throw Error(`unknown accessibilityRole "${r}"`)
  return v
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

export class CraieHost {
  private nextId = 0
  private freeIds: number[] = []
  /** Generation per id; bumped when native frees the slot. */
  private gens: number[] = []
  private seq = 0
  private encoder = new Encoder()
  private scheduled = false
  private flushWaiters = new Map<number, () => void>()
  /** Live nodes by native id — the event-dispatch target table. */
  private nodes = new Map<number, HostNode>()

  constructor(private transport: Transport) {
    transport.onAck?.((seq) => this.ack(seq))
    transport.onEvent?.((ev) => this.dispatchEvent(ev))
  }

  /** Native applied transaction `seq`: resolve its flush waiters. */
  private ack(seq: number) {
    this.flushWaiters.get(seq)?.()
    this.flushWaiters.delete(seq)
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
    const seq = ++this.seq
    this.transport.send(this.encoder.finish(seq))
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
    let mask = 0
    const walk = (n: HostNode, style: TextSpanIn) => {
      mask |= listenerMask(n.props)
      // A hidden root is `display: none` natively and keeps its text, so
      // revealing it needs no recomposition; hidden nested Text drops out.
      if (n !== r && (n.suspended || n.props.hidden)) return
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
          }
          spans.push({ ...style, start })
          owners.push(n)
        }
        text += own
        bytes += utf8Length(own) - (joins ? 2 : 0)
      }
      for (const k of n.textKids ?? []) walk(k, inheritSpan(style, k.props))
    }
    const base = spanStyle(r.props)
    walk(r, base)
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
    const interaction = `${mask},${!!r.props.focusable},${!!r.props.selectable}`
    if (interaction !== r.sentInteraction) {
      r.sentInteraction = interaction
      this.encoder.interaction(r.id, mask, !!r.props.focusable, !!r.props.selectable)
    }
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
    const root = this.nodes.get(ev.node)
    if (!root || root.gen !== ev.generation) return
    // A pointer event on a text root carries the span under the pointer
    // (key bits 16+, 0: none): it goes to the innermost nested Text of
    // that span with a listener for it, else to the root.
    const n = this.spanTarget(root, ev)
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
      case EVENT_KIND.pointerEnter: p.onPointerEnter?.(e); break
      case EVENT_KIND.pointerLeave: p.onPointerLeave?.(e); break
      case EVENT_KIND.wheel: p.onWheel?.({ ...e, dx: ev.a, dy: ev.b }); break
      case EVENT_KIND.keyDown: p.onKeyDown?.({ ...e, key: ev.key, char: ev.text }); break
      case EVENT_KIND.keyUp: p.onKeyUp?.({ ...e, key: ev.key, char: ev.text }); break
      case EVENT_KIND.focus: p.onFocus?.(e); break
      case EVENT_KIND.blur: p.onBlur?.(e); break
      case EVENT_KIND.change: p.onChangeText?.(ev.text); break
      case EVENT_KIND.submit: p.onSubmit?.(ev.text); break
      case EVENT_KIND.scroll: p.onScroll?.({ target: n, x: ev.a, y: ev.b }); break
      case EVENT_KIND.animationEnd: {
        const prop = ev.key & 0xff
        const reason = END_REASON[(ev.key >>> 8) & 0xff] ?? "cancelled"
        const list = root.pendingAnims ?? []
        const i = list.findIndex(p => p.prop === prop)
        if (i >= 0) list.splice(i, 1)[0]!.resolve({ finished: reason === "finished", reason })
        break
      }
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

  private spanTarget(root: HostNode, ev: UiEvent): HostNode {
    const span = ev.key >>> 16
    const handler = POINTER_HANDLER[ev.kind]
    if (!root.spanOwners || span === 0 || !handler) return root
    // Hit-tested against another span table: the span index is stale.
    if (ev.revision !== (root.paragraphRev ?? 0)) return root
    for (let n: HostNode | undefined = root.spanOwners[span - 1]; n; n = n.textParent) {
      if (typeof n.props[handler] === "function") return n
      if (n === root) break
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
    n.paragraphRev = 0
    this.nodes.set(n.id, n)
    if (!this.ready()) return
    const enc = this.encoder
    enc.create(n.id, n.kind)
    this.emitProps(n, {}, n.props, false)
    enc.place(parent ? parent.id : NIL, n.id, before ? before.id : NIL)
    for (const child of n.initial) this.place(n, child, null)
    n.initial = []
  }

  place(parent: HostNode | null, child: HostNode, before: HostNode | null) {
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
   * node's own `release` (via detachDeletedInstance) frees it. */
  detach(n: HostNode) {
    if (n.textParent) {
      this.unlinkVirtual(n)
      return
    }
    if (!n.mounted) return
    if (this.ready()) this.encoder.detach(n.id)
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
    // Its native end events will not reach it (the generation moves).
    for (const p of n.pendingAnims?.splice(0) ?? []) p.resolve({ finished: false, reason: "removed" })
    if (this.ready()) this.encoder.remove(n.id)
    this.gens[n.id] = (this.gens[n.id]! + 1) & 0xffff
    this.freeIds.push(n.id)
    n.mounted = false
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
    const oldTr = mounted ? transitionKey(oldProps.style?.transition) : ""
    const newTr = transitionKey(props.style?.transition)
    if (mounted && oldTr !== newTr) enc.transition(id, props.style?.transition)

    // Layout inputs (spatial keys split off; hiding is display: none).
    const oldLayout = mounted ? layoutOf(oldProps, n.suspended) : undefined
    const newLayout = layoutOf(props, n.suspended)
    if (styleKey(oldLayout) !== styleKey(newLayout)) enc.layout(id, newLayout)

    // Spatial: transform and opacity never touch layout.
    const oldT = transformMatrix(oldProps.style?.transform)
    const newT = transformMatrix(props.style?.transform)
    const oldO = oldProps.style?.opacity ?? 1
    const newO = props.style?.opacity ?? 1
    const tChanged = !sameMatrix(oldT, newT)
    if (tChanged || oldO !== newO) {
      enc.spatial(id, tChanged ? newT : undefined, oldO !== newO ? newO : undefined)
    }

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
      if (oldBg !== newBg || oldR !== newR || oldBc !== newBc || oldBw !== newBw) {
        enc.paint(
          id,
          oldBg !== newBg ? newBg : undefined,
          oldR !== newR ? newR : undefined,
          oldBc !== newBc || oldBw !== newBw ? { color: newBc, width: newBw } : undefined,
        )
      }
    }

    if (!mounted && newTr !== "") enc.transition(id, props.style?.transition)

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

    if (n.kind === 5 && props.asset !== undefined && props.asset !== oldProps.asset) {
      // Vector: the asset bytes (`craie-svg` output), copied once per
      // change (identity compare).
      enc.payload(id, props.asset)
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
      const color32 = color(props.color, 0xffff_ffff)
      const ph = props.placeholder ?? ""
      const multiline = !!props.multiline
      if (
        !mounted ||
        oldProps.fontSize !== props.fontSize ||
        color(oldProps.color, 0xffff_ffff) !== color32 ||
        (oldProps.placeholder ?? "") !== ph ||
        !!oldProps.multiline !== multiline
      ) {
        enc.inputConfig(id, fs, color32, ph, multiline)
      }
      if (!mounted && typeof props.value === "string" && props.value !== "") {
        enc.cmdSetText(id, props.value)
      }
    }

    // Listener mask + focusable flag (a text root's: at the seal).
    const oldMask = listenerMask(oldProps), newMask = listenerMask(props)
    if (
      n.kind !== 1 &&
      (oldMask !== newMask || !!oldProps.focusable !== !!props.focusable ||
        !!oldProps.selectable !== !!props.selectable)
    ) {
      enc.interaction(id, newMask, !!props.focusable, !!props.selectable)
    }

    const oldRole = mounted ? roleOf(oldProps) : ROLE.none
    const newRole = roleOf(props)
    if (oldRole !== newRole) enc.role(id, newRole)

    const oldLabel = oldProps.accessibilityLabel ?? ""
    const newLabel = props.accessibilityLabel ?? ""
    if (oldLabel !== newLabel) enc.label(id, newLabel)
  }
}

