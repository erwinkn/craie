import { test, expect } from "bun:test"
import { readFileSync } from "node:fs"
import { createRoot } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { readFrame } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  acks: ((seq: number) => void)[] = []
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(cb: (seq: number) => void) { this.acks.push(cb) }
  onEvent(_: (ev: UiEvent) => void) {}
  close() {}
}

test("registerFont sends the file in its own transaction and resolves on its ack", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const inter = readFileSync(new URL("../../../assets/fonts/Inter-Subset-Regular.ttf", import.meta.url))
  let done = false
  const p = root.host.registerFont(inter, "Brand Sans").then(() => { done = true })
  expect(t.frames.length).toBe(1)
  const f = readFrame(t.frames[0]!)
  const op = f.ops.find(o => o.tag === 0x74)!
  expect(op.s).toBe("Brand Sans")
  expect(op.bytes!.length).toBe(inter.length)
  expect(Buffer.from(op.bytes!).equals(inter)).toBe(true)
  await Promise.resolve()
  expect(done).toBe(false)
  for (const ack of t.acks) ack(Number(f.seq))
  await p
  expect(done).toBe(true)
  // Without a family: the file's own names.
  root.host.registerFont(new Uint8Array(inter).buffer)
  expect(readFrame(t.frames[1]!).ops.find(o => o.tag === 0x74)!.s).toBeUndefined()
})
