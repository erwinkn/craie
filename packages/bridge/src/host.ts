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

  constructor(private transport: Transport) {}

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
    const buf = this.encoder.finish(++this.seq)
    this.encoder = new Encoder()
    this.transport.send(buf)
  }

  /** Flush any recorded ops now (e.g. before teardown or a checkpoint). */
  flush() {
    this.seal()
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

  remove(n: HostNode) {
    if (!n.mounted) return
    if (this.ready()) this.encoder.remove(n.id)
    // TCP ordering means the remove lands before any create reusing the id.
    this.freeIds.push(n.id)
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
