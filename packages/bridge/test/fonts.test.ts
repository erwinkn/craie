import { test, expect } from "bun:test"
import { readFileSync } from "node:fs"
import { createRoot, MAX_FONT_BYTES } from "../src/index.js"
import type { Transport, UiEvent } from "../src/host.js"
import { readFrame } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  acks: ((seq: number) => void)[] = []
  fail = false
  send(frame: Uint8Array) {
    if (this.fail) throw Error("commit queue is full")
    this.frames.push(frame.slice())
  }
  onAck(cb: (seq: number) => void) { this.acks.push(cb) }
  onEvent(_: (ev: UiEvent) => void) {}
  close() {}
  /** Acks every frame sent so far. */
  ackAll() {
    for (const f of this.frames) for (const ack of this.acks) ack(Number(readFrame(f).seq))
  }
}

const inter = readFileSync(new URL("../../../assets/fonts/Inter-Subset-Regular.ttf", import.meta.url))
const settle = () => new Promise(r => setTimeout(r, 0))
const fontOps = (t: FakeTransport) =>
  t.frames.map(f => readFrame(f).ops.filter(o => o.tag === 0x74))

test("registerFont sends the file alone, once earlier commits are acked, and resolves on its ack", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let done = false
  const p = root.host.registerFont(inter, "Brand Sans").then(() => { done = true })
  await settle()
  // What was pending goes first, without the font.
  expect(fontOps(t)).toEqual([[]])
  t.ackAll()
  await settle()
  expect(t.frames.length).toBe(2)
  const ops = fontOps(t)[1]!
  expect(ops.length).toBe(1)
  expect(ops[0]!.s).toBe("Brand Sans")
  expect(Buffer.from(ops[0]!.bytes!).equals(inter)).toBe(true)
  expect(done).toBe(false)
  t.ackAll()
  await p
  expect(done).toBe(true)
  // Without a family: the file's own names.
  const q = root.host.registerFont(new Uint8Array(inter).buffer)
  await settle(); t.ackAll(); await settle(); t.ackAll(); await q
  expect(fontOps(t).flat().at(-1)!.s).toBeUndefined()
})

test("fonts go one at a time: the second waits for the first's ack", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const a = root.host.registerFont(inter, "A")
  const b = root.host.registerFont(inter, "B")
  for (let i = 0; i < 2; i++) { await settle(); t.ackAll() }
  await settle()
  // The first font is sent and unacked: the second isn't sent yet.
  expect(fontOps(t).flat().map(o => o.s)).toEqual(["A"])
  t.ackAll()
  await a
  for (let i = 0; i < 2; i++) { await settle(); t.ackAll() }
  await b
  expect(fontOps(t).flat().map(o => o.s)).toEqual(["A", "B"])
  for (const f of fontOps(t)) expect(f.length).toBeLessThanOrEqual(1)
})

test("registerFont rejects WOFF, WOFF2, oversized files and transport failures, sending nothing", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  for (const magic of ["wOFF", "wOF2"]) {
    const bytes = new Uint8Array([...magic].map(c => c.charCodeAt(0)).concat([0, 1, 0, 0]))
    await expect(root.host.registerFont(bytes)).rejects.toBeInstanceOf(TypeError)
  }
  await expect(root.host.registerFont(new Uint8Array(MAX_FONT_BYTES + 1))).rejects.toBeInstanceOf(RangeError)
  expect(t.frames.length).toBe(0)
  t.fail = true
  let threw = false
  let p: Promise<void> | undefined
  try { p = root.host.registerFont(inter) } catch { threw = true }
  expect(threw).toBe(false)
  await expect(p!).rejects.toThrow("commit queue is full")
  // A later font still goes once the transport recovers.
  t.fail = false
  const q = root.host.registerFont(inter, "Later")
  for (let i = 0; i < 2; i++) { await settle(); t.ackAll() }
  await q
  expect(fontOps(t).flat().map(o => o.s)).toEqual(["Later"])
})
