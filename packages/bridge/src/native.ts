// In-process native transport: React runs in a worker_thread, the native
// window + event loop own the main thread. Mirrors the craie-node N-API
// surface (crates/node).
//
//   host.ts:   await runApp("<path>/craie_node.node", new URL("./app.tsx", import.meta.url))
//   app.tsx:   const root = attachApp(require("<path>/craie_node.node"))

import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads"
import { createRequire } from "node:module"
import { writeSync } from "node:fs"
import { fileURLToPath } from "node:url"
import { format } from "node:util"
import { createRoot, type Root } from "./index.js"
import type { Transport } from "./host.js"

/** What a JS painter gets for each custom node at paint time: the wire
 * payload plus the node's content rect, in logical points. */
export interface PaintSpec {
  tag: number
  data: number[]
  text: string
  x: number
  y: number
  w: number
  h: number
}

/** One filled rect a painter returns — logical points, 0xRRGGBBAA
 * colors. `radius`/`borderW`/`borderColor` are optional. */
export interface PaintQuad {
  x: number
  y: number
  w: number
  h: number
  color: number
  radius?: number
  borderW?: number
  borderColor?: number
}

export type PainterFn = (spec: PaintSpec) => PaintQuad[]

export interface NativeHostHandle {
  readonly id: number
  run(): string
  close(reason: string): void
  /** Registers the painter for `<Custom>` payloads with `tag`. Must be
   * called before `run`; the callback fires on the UI thread during
   * paint. */
  registerPainter(tag: number, callback: PainterFn): void
}

export interface NativeClientHandle {
  submit(bytes: Uint8Array): void
  /** Registers the UI -> JS frame callback: each call carries one tagged
   * binary frame — tag 0 = ack batch (`u32 count` + `u64 seq`s), tag 1 =
   * event batch (see `decodeEvents`). */
  subscribe(callback: (frame: Uint8Array) => void): void
  close(reason: string): void
}

/** Decodes one events-frame payload into records (tag byte already
 * consumed). Mirror of `events::encode_events`. */
export function decodeEvents(buf: Uint8Array, at = 0): import("./host.js").UiEvent[] {
  const view = new DataView(buf.buffer, buf.byteOffset + at, buf.byteLength - at)
  const count = view.getUint32(0, true)
  let pos = 4
  const out: import("./host.js").UiEvent[] = []
  const text = new TextDecoder()
  for (let i = 0; i < count; i++) {
    const kind = view.getUint8(pos)
    const node = view.getUint32(pos + 4, true)
    const x = view.getFloat32(pos + 8, true)
    const y = view.getFloat32(pos + 12, true)
    const a = view.getFloat32(pos + 16, true)
    const b = view.getFloat32(pos + 20, true)
    const key = view.getUint32(pos + 24, true)
    const len = view.getUint32(pos + 28, true)
    pos += 32
    const s = len ? text.decode(buf.subarray(at + pos, at + pos + len)) : ""
    pos += len
    out.push({ kind, node, x, y, a, b, key, text: s })
  }
  return out
}

export interface Bindings {
  craieRuntimeVersion(): number
  NativeHost: new (options?: { title?: string; width?: number; height?: number }) => NativeHostHandle
  NativeClient: new (id: number) => NativeClientHandle
}

export function loadBindings(path?: string): Bindings {
  const require = createRequire(import.meta.url)
  const resolved =
    path ??
    process.env.CRAIE_NODE ??
    fileURLToPath(new URL("../../../craie-node.node", import.meta.url))
  const bindings = require(resolved) as Bindings
  if (bindings.craieRuntimeVersion() !== 1) throw Error("Craie native bridge protocol mismatch")
  return bindings
}

/** Wire `Transport` over a `NativeClient`: submits go straight to the
 * session queue; acks and UI events arrive pushed through `subscribe`
 * on this worker's event loop — no blocked receive task. */
export class NativeTransport implements Transport {
  private ackCb: ((seq: number) => void) | null = null
  private eventCb: ((ev: import("./host.js").UiEvent) => void) | null = null
  private closed = false

  constructor(private client: NativeClientHandle) {
    client.subscribe((frame) => this.onFrame(frame))
  }

  private onFrame(frame: Uint8Array | null) {
    // The weak threadsafe function may deliver a final teardown call
    // with no frame attached.
    if (this.closed || !frame || frame.length === 0) return
    const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength)
    switch (frame[0]) {
      case 0: {
        const count = view.getUint32(1, true)
        for (let i = 0; i < count; i++) {
          this.ackCb?.(Number(view.getBigUint64(5 + i * 8, true)))
        }
        break
      }
      case 1: {
        const cb = this.eventCb
        if (cb) for (const ev of decodeEvents(frame, 1)) cb(ev)
        break
      }
    }
  }

  send(frame: Uint8Array) {
    try {
      this.client.submit(frame)
    } catch (error) {
      // A dropped transaction corrupts every dependent delta; the session
      // cannot continue. Close it so the native side exits cleanly, then
      // let the error propagate to the caller.
      this.closed = true
      try {
        this.client.close(error instanceof Error ? error.message : String(error))
      } catch {}
      throw error
    }
  }

  onAck(cb: (seq: number) => void) {
    this.ackCb = cb
  }

  onEvent(cb: (ev: import("./host.js").UiEvent) => void) {
    this.eventCb = cb
  }

  close(reason?: string) {
    if (this.closed) return
    this.closed = true
    this.client.close(reason ?? "client closed")
  }
}

/** Worker side: attach to the host session and return a ready root. */
export function attachApp(bindings: Bindings): Root {
  if (isMainThread || workerData?.craieSession === undefined) {
    throw Error("attachApp requires an application worker")
  }
  // The main thread sits inside the native event loop for the app's
  // lifetime, so piped worker stdout never reaches the terminal. Write
  // straight to the fds instead.
  const toFd = (fd: number, args: unknown[]) => writeSync(fd, format(...args) + "\n")
  console.log = (...args: unknown[]) => toFd(1, args)
  console.info = console.log
  console.warn = (...args: unknown[]) => toFd(2, args)
  console.error = console.warn
  const client = new bindings.NativeClient(workerData.craieSession)
  const root = createRoot(new NativeTransport(client))
  const fail = (error: unknown) =>
    client.close(error instanceof Error ? error.message : String(error))
  process.on("uncaughtExceptionMonitor", fail)
  process.on("unhandledRejection", fail)
  parentPort?.on("message", (message) => {
    if (message?.craieShutdown) process.exit(0)
  })
  parentPort?.postMessage({ craieReady: true })
  return root
}

/** Main thread: open the native window, spawn the app worker, run the
 * event loop until the window closes. Never activates the application. */
export async function runApp(
  bindings: Bindings,
  entry: string | URL,
  options: {
    title?: string
    width?: number
    height?: number
    /** Painter tag -> function, registered before the loop starts. */
    painters?: Record<number, PainterFn>
  } = {},
): Promise<void> {
  if (!isMainThread) throw Error("runApp requires the main thread")
  const { painters, ...hostOptions } = options
  const host = new bindings.NativeHost(hostOptions)
  if (painters) {
    for (const [tag, fn] of Object.entries(painters)) {
      host.registerPainter(Number(tag), fn)
    }
  }
  let worker: Worker | undefined
  try {
    worker = new Worker(entry, { workerData: { craieSession: host.id } })
    const stopped = new Promise<void>((resolve) => worker!.once("exit", () => resolve()))
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(Error("Application worker did not attach in 10s")), 10_000)
      worker!.once("error", (e) => { clearTimeout(timer); reject(e) })
      worker!.once("exit", (code) => { clearTimeout(timer); reject(Error(`Worker exited before attach: ${code}`)) })
      worker!.on("message", (m) => { if (m?.craieReady) { clearTimeout(timer); resolve() } })
    })
    const reason = host.run()
    worker.postMessage({ craieShutdown: true })
    await Promise.race([stopped, new Promise((r) => setTimeout(r, 2000))])
    if (reason !== "Native window closed") throw Error(reason)
  } catch (error) {
    host.close(error instanceof Error ? error.message : String(error))
    if (worker) { try { worker.postMessage({ craieShutdown: true }) } catch {}; await worker.terminate().catch(() => {}) }
    throw error
  }
}
