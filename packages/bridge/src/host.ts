// Commit-side host bookkeeping: JS owns node ids, maps element props to
// wire ops, and seals one transaction per commit via queueMicrotask.

import { Encoder, EVENT_KIND, EVENT_MASK, NIL, type StyleProps } from "./wire.js"

export type Kind = 0 | 1 | 2 | 3 // 0 view, 1 text, 2 input, 3 custom — mirror NodeKind
export const KIND: Record<string, Kind> = { view: 0, text: 1, input: 2, custom: 3 }

/** One decoded UI -> JS event record (see events.rs `UiEvent`). */
export interface UiEvent {
  kind: number
  /** Native node id. */
  node: number
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
  type: string
  kind: Kind
  props: Record<string, any>
  mounted: boolean
  root: CraieHost
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
  /** Native acks an applied transaction by seq. Optional: transports that
   * don't ack leave ids unrecycled (safe, leaks the free list). */
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

// Listener prop name -> mask bit; emit PROPS when the mask changes.
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

export class CraieHost {
  private nextId = 0
  private freeIds: number[] = []
  private seq = 0
  private encoder = new Encoder()
  private scheduled = false
  /** Ids removed in the currently-open transaction. */
  private removing: number[] = []
  /** seq -> ids that may be recycled once native acks the txn. */
  private awaitingAck = new Map<number, number[]>()
  private flushWaiters = new Map<number, () => void>()
  /** Live nodes by native id — the event-dispatch target table. */
  private nodes = new Map<number, HostNode>()

  constructor(private transport: Transport) {
    transport.onAck?.((seq) => this.ack(seq))
    transport.onEvent?.((ev) => this.dispatchEvent(ev))
  }

  /** Native applied transaction `seq`: recycle its removed ids and
   * resolve its flush waiters. */
  private ack(seq: number) {
    const ids = this.awaitingAck.get(seq)
    if (ids !== undefined) {
      this.awaitingAck.delete(seq)
      for (const id of ids) this.freeIds.push(id)
    }
    this.flushWaiters.get(seq)?.()
    this.flushWaiters.delete(seq)
  }

  private alloc(): number {
    const free = this.freeIds.pop()
    if (free !== undefined) return free
    if (this.nextId === MAX_ID) throw Error("node ids exhausted")
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
    const buf = this.encoder.finish(seq)
    // Style ids persist across txns; reset() keeps the table.
    this.encoder.reset()
    if (this.removing.length) {
      this.awaitingAck.set(seq, this.removing)
      this.removing = []
    }
    this.transport.send(buf)
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
      id: 0, type, kind, props, mounted: false, root: this, initial: [],
      focus() { this.root.cmd(this.id, (e, id) => e.cmdFocus(id)) },
      blur() { this.root.cmd(this.id, (e, id) => e.cmdBlur(id)) },
      scrollTo(x: number, y: number) {
        this.root.cmd(this.id, (e, id) => e.cmdScrollTo(id, x, y))
      },
      setText(text: string) {
        this.nativeText = text
        this.root.cmd(this.id, (e, id) => e.cmdSetInputText(id, text))
      },
    }
    return n
  }

  /** Emits an imperative command op in the current transaction. */
  cmd(id: number, write: (enc: Encoder, id: number) => void) {
    if (!id) return
    if (this.ready()) write(this.encoder, id)
  }

  /** Routes a native event record to the target node's listener props. */
  private dispatchEvent(ev: UiEvent) {
    const n = this.nodes.get(ev.node)
    if (!n) return
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
    n.mounted = true
    this.nodes.set(n.id, n)
    if (!this.ready()) return
    const enc = this.encoder
    enc.create(n.id, n.kind)
    this.emitProps(n, {}, n.props)
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

  /** React deleted `n` for good: free the native slot. The id is held
   * until the removing transaction is acknowledged, then recycled. */
  release(n: HostNode) {
    if (!n.mounted) return
    this.nodes.delete(n.id)
    if (this.ready()) this.encoder.remove(n.id)
    this.removing.push(n.id)
    n.mounted = false
  }

  setHidden(n: HostNode, hidden: boolean) {
    if (this.ready()) this.encoder.hidden(n.id, hidden)
  }

  /** Prop diff -> ops for the fields that changed. */
  update(n: HostNode, oldProps: Record<string, any>, props: Record<string, any>) {
    n.props = props
    if (this.ready()) this.emitProps(n, oldProps, props)
  }

  setTextContent(n: HostNode, text: string) {
    n.props = { ...n.props, children: text }
    if (this.ready()) this.encoder.setText(n.id, text)
  }

  /** Writes the ops for props that differ between old and next. */
  private emitProps(n: HostNode, oldProps: Record<string, any>, props: Record<string, any>) {
    const enc = this.encoder

    // style (interned; identical objects share one wire definition)
    const oldKey = oldProps.style ? canon(oldProps.style) : ""
    const newKey = props.style ? canon(props.style) : ""
    if (oldKey !== newKey) enc.setStyle(n.id, enc.styleIdFor(props.style))

    if (n.kind === 1) {
      const oldText = textOf(oldProps), newText = textOf(props)
      if (oldText !== newText) enc.setText(n.id, newText)
      const fs = props.fontSize ?? 14, color32 = color(props.color, 0xffff_ffff)
      if (oldProps.fontSize !== props.fontSize || color(oldProps.color, 0xffff_ffff) !== color32) {
        enc.textProps(n.id, fs, color32)
      }
    } else {
      // Paint record: fill, corner radius, border — each an optional
      // masked field; emit only what changed.
      const oldBg = color(oldProps.backgroundColor), newBg = color(props.backgroundColor)
      const oldR = oldProps.borderRadius ?? 0, newR = props.borderRadius ?? 0
      const oldBc = color(oldProps.borderColor), newBc = color(props.borderColor)
      const oldBw = oldProps.borderWidth ?? 0, newBw = props.borderWidth ?? 0
      if (oldBg !== newBg || oldR !== newR || oldBc !== newBc || oldBw !== newBw) {
        enc.paint(
          n.id,
          oldBg !== newBg ? newBg : undefined,
          oldR !== newR ? newR : undefined,
          oldBc !== newBc || oldBw !== newBw
            ? { color: newBc, width: newBw }
            : undefined,
        )
      }
    }

    if (n.kind === 3) {
      // Custom-element payload; `data` compares by content.
      const tag = props.tag ?? 0
      const text = props.text ?? ""
      const data: readonly number[] = props.data ?? []
      const oldData: readonly number[] = oldProps.data ?? []
      const sameData =
        data.length === oldData.length && data.every((v, i) => v === oldData[i])
      if (tag !== (oldProps.tag ?? 0) || text !== (oldProps.text ?? "") || !sameData) {
        enc.custom(n.id, tag, data, text)
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
        oldProps.fontSize !== props.fontSize ||
        color(oldProps.color, 0xffff_ffff) !== color32 ||
        (oldProps.placeholder ?? "") !== ph ||
        !!oldProps.multiline !== multiline
      ) {
        enc.inputProps(n.id, fs, color32, ph, multiline)
      }
      if (
        typeof props.value === "string" &&
        props.value !== oldProps.value &&
        props.value !== n.nativeText
      ) {
        n.nativeText = props.value
        enc.cmdSetInputText(n.id, props.value)
      }
    }

    // Listener mask + focusable flag.
    const oldMask = listenerMask(oldProps), newMask = listenerMask(props)
    if (oldMask !== newMask || !!oldProps.focusable !== !!props.focusable) {
      enc.props(n.id, newMask, !!props.focusable)
    }

    if (!!oldProps.hidden !== !!props.hidden) enc.hidden(n.id, !!props.hidden)

    const oldLabel = oldProps.accessibilityLabel ?? ""
    const newLabel = props.accessibilityLabel ?? ""
    if (oldLabel !== newLabel) enc.label(n.id, newLabel)
  }
}

function canon(v: unknown): string {
  if (v === null || typeof v !== "object") return JSON.stringify(v)!
  if (Array.isArray(v)) return `[${v.map(canon).join(",")}]`
  const o = v as Record<string, unknown>
  return `{${Object.keys(o).sort().map(k => `${JSON.stringify(k)}:${canon(o[k])}`).join(",")}}`
}
