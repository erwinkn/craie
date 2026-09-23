// Commit-side host bookkeeping: JS owns node ids, maps element props to
// CRW2 ops, and seals one transaction per commit via queueMicrotask.
//
// Ids recycle immediately on removal. Native bumps a slot's generation
// when it frees it; JS mirrors the counter, and events carry the
// generation, so an event for a previous occupant of an id is dropped.

import {
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
  type StyleProps,
  type TextSpanIn,
} from "./wire.js"

export type Kind = 0 | 1 | 2 | 3 // 0 view, 1 text, 2 input, 3 surface — mirror NodeKind
export const KIND: Record<string, Kind> = { view: 0, text: 1, input: 2, surface: 3 }

/** One decoded UI -> JS event record (see events.rs `UiEvent`). */
export interface UiEvent {
  kind: number
  /** Native node id. */
  node: number
  /** The node's generation when the event fired. */
  generation: number
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
  /** Last buffer the native input reported; `value` commands that match
   * it are echoes of native edits and are not sent back. */
  nativeText?: string
  focus(): void
  blur(): void
  scrollTo(x: number, y: number): void
  /** Replaces an input node's buffer (the `value` command). */
  setText(text: string): void
}

export interface Transport {
  send(frame: Uint8Array): void
  close(reason?: string): void
  /** Native acks an applied transaction by seq; resolves `flush()`. */
  onAck?(cb: (seq: number) => void): void
  /** Native pushes UI events (pointer/key/focus/input/scroll). */
  onEvent?(cb: (ev: UiEvent) => void): void
}

const MAX_ID = 0xffff_fffd

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

/** Span zero of a text node: the base style. */
function baseSpan(props: Record<string, any>): TextSpanIn {
  return {
    start: 0,
    fontSize: props.fontSize ?? 14,
    color: color(props.color, 0xffff_ffff),
    weight: typeof props.fontWeight === "number"
      ? props.fontWeight
      : props.fontWeight === "bold" ? 700 : 400,
    italic: props.fontStyle === "italic",
  }
}

function sameSpan(a: TextSpanIn, b: TextSpanIn): boolean {
  return a.fontSize === b.fontSize && a.color === b.color &&
    a.weight === b.weight && !!a.italic === !!b.italic
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
    const seq = ++this.seq
    this.transport.send(this.encoder.finish(seq))
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
        this.nativeText = text
        this.root.cmd(this, (e, id) => e.cmdSetText(id, text))
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
    const n = this.nodes.get(ev.node)
    if (!n || n.gen !== ev.generation) return
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
      case EVENT_KIND.change:
        n.nativeText = ev.text
        p.onChangeText?.(ev.text)
        break
      case EVENT_KIND.submit: p.onSubmit?.(ev.text); break
      case EVENT_KIND.scroll: p.onScroll?.({ target: n, x: ev.a, y: ev.b }); break
    }
  }

  /** Emits create + props + placement for a subtree root. Children that
   * arrived before mount are replayed by the reconciler's place calls. */
  materialize(n: HostNode, parent: HostNode | null, before: HostNode | null) {
    if (n.mounted) return
    n.id = this.alloc()
    n.gen = this.gens[n.id]!
    n.mounted = true
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
    if (!child.mounted) {
      this.materialize(child, parent, before)
      return
    }
    if (this.ready()) {
      this.encoder.place(parent ? parent.id : NIL, child.id, before ? before.id : NIL)
    }
  }

  /** Unlinks `n` without freeing its slot — removal unmounts it; the
   * node's own `release` (via detachDeletedInstance) frees it. */
  detach(n: HostNode) {
    if (!n.mounted) return
    if (this.ready()) this.encoder.detach(n.id)
  }

  /** React deleted `n` for good: free the native slot and recycle the
   * id at once. The generation bump keeps stale events out. */
  release(n: HostNode) {
    if (!n.mounted) return
    this.nodes.delete(n.id)
    if (this.ready()) this.encoder.remove(n.id)
    this.gens[n.id] = (this.gens[n.id]! + 1) & 0xffff
    this.freeIds.push(n.id)
    n.mounted = false
  }

  /** React (Suspense) hides or reveals a node: `display: none`. */
  setSuspended(n: HostNode, hidden: boolean) {
    if (n.suspended === hidden) return
    const before = styleKey(layoutOf(n.props, n.suspended))
    n.suspended = hidden
    const layout = layoutOf(n.props, n.suspended)
    if (styleKey(layout) !== before && this.ready()) this.encoder.layout(n.id, layout)
  }

  /** Prop diff -> ops for the fields that changed. */
  update(n: HostNode, oldProps: Record<string, any>, props: Record<string, any>) {
    n.props = props
    if (this.ready()) this.emitProps(n, oldProps, props, true)
  }

  setTextContent(n: HostNode, text: string) {
    const old = n.props
    n.props = { ...n.props, text, children: undefined }
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
      const oldText = textOf(oldProps), newText = textOf(props)
      const oldSpan = baseSpan(oldProps), newSpan = baseSpan(props)
      if (!mounted || oldText !== newText || !sameSpan(oldSpan, newSpan)) {
        enc.paragraph(id, newText, [newSpan])
      }
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

    if (n.kind === 2) {
      // INPUT config; the buffer itself only moves through `value`
      // commands (echoes of native edits are filtered by nativeText).
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
      if (
        typeof props.value === "string" &&
        props.value !== oldProps.value &&
        props.value !== n.nativeText
      ) {
        n.nativeText = props.value
        enc.cmdSetText(id, props.value)
      }
    }

    // Listener mask + focusable flag.
    const oldMask = listenerMask(oldProps), newMask = listenerMask(props)
    if (oldMask !== newMask || !!oldProps.focusable !== !!props.focusable) {
      enc.interaction(id, newMask, !!props.focusable)
    }

    const oldRole = mounted ? roleOf(oldProps) : ROLE.none
    const newRole = roleOf(props)
    if (oldRole !== newRole) enc.role(id, newRole)

    const oldLabel = oldProps.accessibilityLabel ?? ""
    const newLabel = props.accessibilityLabel ?? ""
    if (oldLabel !== newLabel) enc.label(id, newLabel)
  }
}

