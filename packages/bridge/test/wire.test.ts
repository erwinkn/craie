import { test, expect } from "bun:test"
import { readFileSync } from "node:fs"
import { Encoder, NIL, ROLE, VERSION, transformMatrix } from "../src/wire.js"
import { readFrame } from "./crw2.js"

// Hand-computed bytes for: create(0, view) | paragraph(0, "hi") | place.
test("encoder emits the documented byte layout", () => {
  const enc = new Encoder()
  enc.create(0, 1)
  enc.paragraph(0, "hi", [{ start: 0, fontSize: 14, color: 0xffffffff }])
  enc.place(NIL, 0, NIL)
  const buf = enc.finish(7n)

  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  let at = 0
  expect(dv.getUint32(at, true)).toBe(0x3257_5243); at += 4 // "CRW2"
  expect(dv.getUint16(at, true)).toBe(18); at += 2            // version
  expect(dv.getUint16(at, true)).toBe(0); at += 2            // flags
  expect(dv.getBigUint64(at, true)).toBe(7n); at += 8        // seq
  expect(dv.getUint32(at, true)).toBe(1); at += 4            // 1 string
  expect(dv.getUint32(at, true)).toBe(0); at += 4            // 0 styles
  expect(dv.getUint32(at, true)).toBe(1); at += 4            // 1 span
  expect(dv.getUint32(at, true)).toBe(2); at += 4            // len 2
  expect(String.fromCharCode(buf[at]!, buf[at + 1]!)).toBe("hi"); at += 2
  // span (28 bytes): start 0, size 14, color, weight 400, flags 0,
  // reserved, family NIL, letter spacing 0, line height 0
  expect(dv.getUint32(at, true)).toBe(0); at += 4
  expect(dv.getFloat32(at, true)).toBe(14); at += 4
  expect(dv.getUint32(at, true)).toBe(0xffffffff); at += 4
  expect(dv.getUint16(at, true)).toBe(400); at += 2
  expect(buf[at++]).toBe(0)
  expect(buf[at++]).toBe(0)
  expect(dv.getUint32(at, true)).toBe(NIL); at += 4
  expect(dv.getFloat32(at, true)).toBe(0); at += 4
  expect(dv.getFloat32(at, true)).toBe(0); at += 4
  // create(0, kind 1)
  expect(buf[at++]).toBe(0x01)
  expect(dv.getUint32(at, true)).toBe(0); at += 4
  expect(buf[at++]).toBe(1)
  // paragraph(0, str 0, span 0, count 1)
  expect(buf[at++]).toBe(0x40)
  for (const v of [0, 0, 0, 1]) { expect(dv.getUint32(at, true)).toBe(v); at += 4 }
  // place(NIL, 0, NIL)
  expect(buf[at++]).toBe(0x02)
  for (const v of [NIL, 0, NIL]) { expect(dv.getUint32(at, true)).toBe(v); at += 4 }
  expect(at).toBe(buf.byteLength)
})

test("style table interns per transaction and resets after finish", () => {
  const enc = new Encoder()
  enc.layout(0, { width: 10, padding: 4 })
  enc.layout(1, { padding: 4, width: 10 }) // same style, other key order
  enc.layout(2, { display: "flex", flexDirection: "column", padding: { left: 8 }, flexGrow: 2 })
  const f = readFrame(enc.finish(1n))
  expect(f.styleCount).toBe(2)
  expect(f.ops.map(o => o.f[0])).toEqual([0, 0, 1])
  // Nothing persists: the next transaction starts a fresh table.
  enc.layout(3, { width: 10, padding: 4 })
  const g = readFrame(enc.finish(2n))
  expect(g.styleCount).toBe(1)
  expect(g.ops[0]!.f[0]).toBe(0)
})

test("style op carries a presence mask and positional fields", () => {
  const enc = new Encoder()
  enc.layout(0, { display: "flex", flexDirection: "column", padding: { left: 8 }, flexGrow: 2 })
  const buf = enc.finish(1n)
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  let at = 28 // header, no strings
  const mask = dv.getBigUint64(at, true); at += 8
  expect(mask).toBe((1n << 0n) | (1n << 2n) | (1n << 12n) | (1n << 17n))
  expect(buf[at++]).toBe(0) // display flex
  expect(buf[at++]).toBe(1) // flexDirection column
  expect(buf[at++]).toBe(0); expect(dv.getFloat32(at, true)).toBe(8); at += 4
  for (let i = 0; i < 3; i++) {
    expect(buf[at++]).toBe(0); expect(dv.getFloat32(at, true)).toBe(0); at += 4
  }
  expect(dv.getFloat32(at, true)).toBe(2)
})

test("unset margin sides are zero; unset inset sides are auto", () => {
  const enc = new Encoder()
  enc.layout(0, { margin: { left: 48 }, inset: { top: 4 } })
  const buf = enc.finish(1n)
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  let at = 28
  const mask = dv.getBigUint64(at, true); at += 8
  expect(mask).toBe((1n << 13n) | (1n << 15n))
  // margin [left, right, top, bottom]: length 48, then three zeros.
  expect(buf[at++]).toBe(0); expect(dv.getFloat32(at, true)).toBe(48); at += 4
  for (let i = 0; i < 3; i++) {
    expect(buf[at++]).toBe(0); expect(dv.getFloat32(at, true)).toBe(0); at += 4
  }
  // inset: left auto, right auto, top 4, bottom auto.
  expect(buf[at++]).toBe(2)
  expect(buf[at++]).toBe(2)
  expect(buf[at++]).toBe(0); expect(dv.getFloat32(at, true)).toBe(4); at += 4
  expect(buf[at++]).toBe(2)
})

test("transform lists fold like CSS", () => {
  const m = transformMatrix([{ translateX: 10 }, { scale: 2 }])
  // Scale applies first, then the translation.
  expect(m).toEqual([2, 0, 0, 2, 10, 0])
  const r = transformMatrix([{ rotate: "90deg" }])
  expect(r[0]).toBeCloseTo(0)
  expect(r[1]).toBeCloseTo(1)
})

test("payload copies typed-array bytes", () => {
  const enc = new Encoder()
  const values = new Float32Array([0.5, 1])
  enc.payload(3, values)
  const f = readFrame(enc.finish(1n))
  const op = f.ops[0]!
  expect(op.tag).toBe(0x71)
  expect(new Float32Array(op.bytes!.buffer)).toEqual(values)
})

// S3C-07: span lists intern by an unambiguous key. Under a joined-field
// key, one span whose family spells a row boundary collided with two
// default spans ("foo" at 0, "bar" at 1): the second paragraph reused
// the first's single row while claiming two.
test("span lists with delimiter-like families do not share rows", () => {
  const enc = new Encoder()
  const base = { fontSize: 14, color: 0xffffffff }
  enc.paragraph(1, "ab", [{ ...base, start: 0, fontFamily: "foo;1,14,4294967295,400,0,0,0,0,bar" }])
  enc.paragraph(2, "ab", [
    { ...base, start: 0, fontFamily: "foo" },
    { ...base, start: 1, fontFamily: "bar" },
  ])
  const f = readFrame(enc.finish(1n))
  const rows = (id: number) => {
    const [start, count] = f.ops.find(o => o.tag === 0x40 && o.id === id)!.f as [number, number]
    return f.spans.slice(start, start + count).map(s => [s.start, s.family])
  }
  expect(rows(1)).toEqual([[0, "foo;1,14,4294967295,400,0,0,0,0,bar"]])
  expect(rows(2)).toEqual([[0, "foo"], [1, "bar"]])
  // Equal lists still share rows.
  enc.paragraph(3, "ab", [{ ...base, start: 0, fontFamily: "foo" }, { ...base, start: 1, fontFamily: "bar" }])
  enc.paragraph(4, "ab", [{ ...base, start: 0, fontFamily: "foo" }, { ...base, start: 1, fontFamily: "bar" }])
  const g = readFrame(enc.finish(2n))
  const ref = (id: number) => g.ops.find(o => o.tag === 0x40 && o.id === id)!.f
  expect(ref(3)).toEqual(ref(4))
  expect(g.spans.length).toBe(2)
})

// S4-10: a bad timing throws before the encoder writes a byte.
test("bad timings leave the encoder unchanged", () => {
  const enc = new Encoder()
  enc.create(1, 0)
  expect(() => enc.transition(1, { opacity: { duration: 100 }, width: { duration: 100, easing: [2, 0, 1, 1] } })).toThrow()
  expect(() => enc.animate(1, "opacity", [0], { spring: { damping: 0 } })).toThrow()
  expect(() => enc.animate(1, "opacity", [0, 1], { duration: 1 })).toThrow()
  enc.place(NIL, 1, NIL)
  const f = readFrame(enc.finish(1n))
  expect(f.ops.map(o => o.tag)).toEqual([0x01, 0x02])
})

// The handshake compares each side's wire VERSION. They agree through
// the fixture: its header must be this VERSION, and Rust's
// wire_fixture test decodes it only at the Rust VERSION.
test("borders per side bumped the protocol to 18 (registered fonts took 17)", () => {
  expect(VERSION).toBe(18)
  expect([ROLE.dialog, ROLE.alertdialog, ROLE.tab, ROLE.tablist]).toEqual([17, 18, 19, 20])
})

test("the cross-language fixture carries this VERSION", () => {
  const buf = readFileSync(new URL("./fixture.bin", import.meta.url))
  expect(buf.readUInt32LE(0)).toBe(0x3257_5243)
  expect(buf.readUInt16LE(4)).toBe(VERSION)
})
