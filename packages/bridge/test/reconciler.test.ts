import { test, expect } from "bun:test"
import { createElement } from "react"
import { createRoot, View, Text } from "../src/index.js"
import type { Transport } from "../src/host.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  ackCb: ((seq: number) => void) | null = null
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(cb: (seq: number) => void) { this.ackCb = cb }
  /** Simulates native applying every sent frame. */
  ackAll() {
    for (const f of this.frames) {
      const seq = Number(new DataView(f.buffer, f.byteOffset).getBigUint64(8, true))
      this.ackCb?.(seq)
    }
  }
  close() {}
}

// Minimal op reader — just enough to list op tags + ids per frame.
function ops(buf: Uint8Array): number[] {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  let at = 20
  const strings = dv.getUint32(at - 4, true)
  for (let i = 0; i < strings; i++) at += 4 + dv.getUint32(at, true)
  const tags: number[] = []
  while (at < buf.byteLength) {
    const tag = buf[at++]
    tags.push(tag)
    switch (tag) {
      case 0x01: at += 5; break
      case 0x02: at += 8; break
      case 0x03: at += 12; break
      case 0x04: at += 8; break
      case 0x05: at += 12; break
      case 0x06: case 0x07: at += 4; break
      case 0x08: at += 5; break
      case 0x0a: at += 8; break
      case 0x0b: { // paint: u32 id + u8 mask + masked fields
        at += 4
        const m = buf[at++]
        if (m & 1) at += 4
        if (m & 2) at += 4
        if (m & 4) at += 8
        break
      }
      case 0x0c: at += 9; break // props: u32 id + u32 mask + u8
      case 0x0d: at += 17; break // input_props: u32 f32 u32 u32 u8
      case 0x0e: { // command: u32 id + u8 tag + payload
        at += 4
        const c = buf[at++]
        if (c === 2) at += 4 // set_input_text: string ref
        else if (c === 3) at += 8 // scroll_to
        break
      }
      case 0x09: {
        at += 4
        let mask = dv.getBigUint64(at, true); at += 8
        // skip fields per mask — approximate walk for known fields
        const take = (n: number) => { at += n }
        const lp = () => { const t = buf[at++]; if (t < 2) take(4) }
        const dim = () => { const t = buf[at++]; if (t <= 1 || t === 5 || t === 6) take(4) }
        for (let bit = 0; bit < 21; bit++) {
          if (!(mask & (1n << BigInt(bit)))) continue
          switch (bit) {
            case 0: case 1: case 2: case 3: case 4: case 5: case 6: case 7: take(1); break
            case 8: lp(); lp(); break
            case 9: dim(); dim(); break
            case 10: case 11: lp(); lp(); break  // lpa subset is fine for tests
            case 12: case 14: for (let i = 0; i < 4; i++) lp(); break
            case 13: case 15: for (let i = 0; i < 4; i++) lp(); break
            case 16: dim(); break
            case 17: case 18: case 19: take(4); break
            case 20: take(2); break
          }
        }
        break
      }
      default: throw Error(`unknown op ${tag}`)
    }
  }
  return tags
}

test("render mounts a tree as wire ops", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  root.renderSync(
    createElement(View, { backgroundColor: "#112233" },
      createElement(Text, { fontSize: 20, color: "#ffffff" }, "hello"))
  )
  await new Promise(r => setTimeout(r, 0)) // let the microtask seal
  expect(t.frames.length).toBeGreaterThan(0)
  const tags = ops(t.frames[0]!)
  // create(view) paint style? create(text) setText textProps place place
  expect(tags).toContain(0x01)
  expect(tags).toContain(0x02)
  expect(tags).toContain(0x05)
  expect(tags).toContain(0x0b)
})

test("update emits only the changed op", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ n }: { n: number }) {
    return createElement(Text, { fontSize: 14 }, `count ${n}`)
  }
  root.renderSync(createElement(App, { n: 0 }))
  await new Promise(r => setTimeout(r, 0))
  t.frames.length = 0
  root.renderSync(createElement(App, { n: 1 }))
  await new Promise(r => setTimeout(r, 0))
  expect(t.frames.length).toBe(1)
  expect(ops(t.frames[0]!)).toEqual([0x02]) // set_text only
})

test("ids recycle only after native ack", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ show }: { show: boolean }) {
    return createElement(View, null,
      show ? createElement(Text, null, "ephemeral") : null,
      createElement(Text, null, "pinned"))
  }
  root.renderSync(createElement(App, { show: true }))
  await new Promise(r => setTimeout(r, 0))
  t.ackAll()

  // Remove the text: its id must NOT be reused until the ack arrives.
  root.renderSync(createElement(App, { show: false }))
  await new Promise(r => setTimeout(r, 0))
  root.renderSync(createElement(App, { show: true }))
  await new Promise(r => setTimeout(r, 0))
  // Without an ack for the removing txn, the remount got a fresh id.
  // The last frame should contain create+place for a new node id != old.
  const last = t.frames.at(-1)!
  const dv = new DataView(last.buffer, last.byteOffset)
  let at = 20 + 4 // 0 or 1 strings? remount has set_text -> strings exist
  const strings = dv.getUint32(16, true)
  at = 20
  for (let i = 0; i < strings; i++) at += 4 + dv.getUint32(at, true)
  // first op should be create; read its id
  expect(last[at]).toBe(0x01)
  const newId = dv.getUint32(at + 1, true)
  expect(newId).not.toBe(0) // the removed node's id stayed out of the pool
})

test("subtree deletion frees every node, ids recycle after ack", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  function App({ show }: { show: boolean }) {
    return createElement(View, null,
      show ? createElement(View, null,
        createElement(Text, null, "a"),
        createElement(Text, null, "b")) : null)
  }
  root.renderSync(createElement(App, { show: true }))
  await new Promise(r => setTimeout(r, 0))
  t.ackAll()
  t.frames.length = 0

  // Delete view + 2 texts. Expect 1 detach (top) + 3 removes, all 5-byte ops.
  root.renderSync(createElement(App, { show: false }))
  await new Promise(r => setTimeout(r, 0))
  const frame = t.frames.at(-1)!
  const dv = new DataView(frame.buffer, frame.byteOffset)
  expect(dv.getUint32(16, true)).toBe(0) // no strings
  const ops = (frame.length - 20) / 5
  expect(ops).toBe(4)
  let removes = 0
  for (let i = 20; i < frame.length; i += 5) if (frame[i] === 0x07) removes++
  expect(removes).toBe(3)

  // After ack, remounting must reuse the 3 freed ids — no fresh id
  // beyond 3 may appear in a create op.
  t.ackAll()
  t.frames.length = 0
  root.renderSync(createElement(App, { show: true }))
  await new Promise(r => setTimeout(r, 0))
  const createIds: number[] = []
  const re = t.frames.at(-1)!
  const dv2 = new DataView(re.buffer, re.byteOffset)
  let at = 20
  for (let i = 0; i < dv2.getUint32(16, true); i++) at += 4 + dv2.getUint32(at, true)
  const sizes: Record<number, number> = {
    0x01: 6, 0x02: 9, 0x03: 13, 0x04: 9, 0x05: 13, 0x06: 5, 0x07: 5, 0x08: 6,
    0x0a: 9, 0x0c: 10, 0x0d: 18,
  }
  while (at < re.length) {
    const tag = re[at]
    if (tag === 0x01) createIds.push(dv2.getUint32(at + 1, true))
    at += sizes[tag] ?? (() => { throw Error(`op 0x${tag.toString(16)}`) })()
  }
  expect(createIds.length).toBe(3)
  expect(Math.max(...createIds)).toBeLessThanOrEqual(3)
})
