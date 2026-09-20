// Commit-side host bookkeeping: JS owns node ids, maps element props to
// wire ops, and seals one transaction per commit via queueMicrotask.

import { Encoder, NIL, type StyleProps } from "./wire.js"

export type Kind = 0 | 1 // 0 view, 1 text — mirror NodeKind in the host
export const KIND: Record<string, Kind> = { view: 0, text: 1 }

export interface HostNode {
  id: number
  type: string
  kind: Kind
  props: Record<string, any>
  mounted: boolean
  root: CraieHost
  /** Children appended before this node got an id; replayed on materialize. */
  initial: HostNode[]
}

export interface Transport {
  send(frame: Uint8Array): void
  close(reason?: string): void
  /** Native acks an applied transaction by seq. Optional: transports that
   * don't ack leave ids unrecycled (safe, leaks the free list). */
  onAck?(cb: (seq: number) => void): void
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

  constructor(private transport: Transport) {
    transport.onAck?.((seq) => this.ack(seq))
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
    return { id: 0, type, kind, props, mounted: false, root: this, initial: [] }
  }

  /** Emits create + props + placement for a subtree root. Children that
   * arrived before mount are replayed by the reconciler's place calls. */
  materialize(n: HostNode, parent: HostNode | null, before: HostNode | null) {
    if (n.mounted) return
    n.id = this.alloc()
    n.mounted = true
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
      const oldBg = color(oldProps.backgroundColor), newBg = color(props.backgroundColor)
      if (oldBg !== newBg) enc.viewPaint(n.id, newBg)
    }

    if (!!oldProps.hidden !== !!props.hidden) enc.hidden(n.id, !!props.hidden)
  }
}

function canon(v: unknown): string {
  if (v === null || typeof v !== "object") return JSON.stringify(v)!
  if (Array.isArray(v)) return `[${v.map(canon).join(",")}]`
  const o = v as Record<string, unknown>
  return `{${Object.keys(o).sort().map(k => `${JSON.stringify(k)}:${canon(o[k])}`).join(",")}}`
}
