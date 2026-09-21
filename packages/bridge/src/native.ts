// In-process native transport: React runs in a worker_thread, the native
// window + event loop own the main thread. Mirrors the craie-node N-API
// surface (crates/node).
//
//   host.ts:   await runApp("<path>/craie_node.node", new URL("./app.tsx", import.meta.url))
//   app.tsx:   const root = attachApp(require("<path>/craie_node.node"))

import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads"
import { createRequire } from "node:module"
import { fileURLToPath } from "node:url"
import { createRoot, type Root } from "./index.js"
import type { Transport } from "./host.js"

export interface NativeHostHandle {
  readonly id: number
  run(): string
  close(reason: string): void
}

export interface NativeClientHandle {
  submit(bytes: Uint8Array): void
  receive(): Promise<number[]>
  close(reason: string): void
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

/** Wire `Transport` over a `NativeClient`: submit copies once, acks
 * arrive through the blocking `receive` pump. */
export class NativeTransport implements Transport {
  private ackCb: ((seq: number) => void) | null = null
  private closed = false

  constructor(private client: NativeClientHandle) {
    void this.pump()
  }

  private async pump() {
    while (!this.closed) {
      const seqs = await this.client.receive()
      if (!seqs.length) return // closed
      for (const seq of seqs) this.ackCb?.(seq)
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
  options: { title?: string; width?: number; height?: number } = {},
): Promise<void> {
  if (!isMainThread) throw Error("runApp requires the main thread")
  const host = new bindings.NativeHost(options)
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
