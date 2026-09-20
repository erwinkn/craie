import { test, expect } from "bun:test"
import { Encoder, NIL } from "../src/wire.js"

// Hand-computed expected bytes for a minimal transaction:
//   create(0, view) | set_text(0, "hi") | place(NIL, 0, NIL)
test("encoder emits the documented byte layout", () => {
  const enc = new Encoder()
  enc.create(0, 0)
  enc.setText(0, "hi")
  enc.place(NIL, 0, NIL)
  const buf = enc.finish(7n)

  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  let at = 0
  expect(dv.getUint32(at, true)).toBe(0x3157_5243); at += 4 // "CRW1"
  expect(dv.getUint16(at, true)).toBe(1); at += 2          // version
  expect(dv.getUint16(at, true)).toBe(0); at += 2          // flags
  expect(dv.getBigUint64(at, true)).toBe(7n); at += 8      // seq
  expect(dv.getUint32(at, true)).toBe(1); at += 4          // 1 string
  expect(dv.getUint32(at, true)).toBe(2); at += 4          // len 2
  expect(String.fromCharCode(buf[at], buf[at + 1])).toBe("hi"); at += 2

  // create(0, kind 0)
  expect(buf[at++]).toBe(0x01)
  expect(dv.getUint32(at, true)).toBe(0); at += 4
  expect(buf[at++]).toBe(0)

  // set_text(0, str 0)
  expect(buf[at++]).toBe(0x02)
  expect(dv.getUint32(at, true)).toBe(0); at += 4
  expect(dv.getUint32(at, true)).toBe(0); at += 4

  // place(NIL, 0, NIL)
  expect(buf[at++]).toBe(0x05)
  expect(dv.getUint32(at, true)).toBe(NIL); at += 4
  expect(dv.getUint32(at, true)).toBe(0); at += 4
  expect(dv.getUint32(at, true)).toBe(NIL); at += 4
  expect(at).toBe(buf.byteLength)
})

test("style op carries a presence mask and positional fields", () => {
  const enc = new Encoder()
  const id = enc.styleIdFor({
    display: "flex",
    flexDirection: "column",
    padding: { left: 8 },
    flexGrow: 2,
  })
  expect(id).toBe(0)
  const buf = enc.finish(1n)
  // Find the style op: last record in the stream.
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  // header 20 bytes, 0 strings, then style op
  let at = 20
  expect(buf[at++]).toBe(0x09)
  expect(dv.getUint32(at, true)).toBe(0); at += 4 // wire id
  const mask = dv.getBigUint64(at, true); at += 8
  // display | flexDirection | padding | flexGrow
  expect(mask).toBe((1n << 0n) | (1n << 2n) | (1n << 12n) | (1n << 17n))
  expect(buf[at++]).toBe(0)            // display flex
  expect(buf[at++]).toBe(1)            // flexDirection column
  // padding l,r,t,b as LP: 8px len, then three zero-len
  expect(buf[at++]).toBe(0); expect(dv.getFloat32(at, true)).toBe(8); at += 4
  for (let i = 0; i < 3; i++) {
    expect(buf[at++]).toBe(0); expect(dv.getFloat32(at, true)).toBe(0); at += 4
  }
  expect(dv.getFloat32(at, true)).toBe(2); at += 4 // flexGrow
  expect(at).toBe(buf.byteLength)
})

test("style interning dedupes identical objects", () => {
  const enc = new Encoder()
  const a = enc.styleIdFor({ width: 10, padding: 4 })
  const b = enc.styleIdFor({ padding: 4, width: 10 }) // different key order
  expect(a).toBe(b)
})
