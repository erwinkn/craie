// Minimal CRW2 reader for tests: header, tables, and op records.

export interface Op {
  tag: number
  id: number
  /** Op-specific fields, in wire order. */
  f: number[]
  /** String operand (text, label, placeholder, set-text command). */
  s?: string
  bytes?: Uint8Array
}

export interface Frame {
  seq: bigint
  strings: string[]
  styleCount: number
  spans: { start: number; fontSize: number; color: number; weight: number; italic: boolean }[]
  ops: Op[]
}

export function readFrame(buf: Uint8Array): Frame {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  let at = 0
  const u8 = () => buf[at++]!
  const u16 = () => { const v = dv.getUint16(at, true); at += 2; return v }
  const u32 = () => { const v = dv.getUint32(at, true); at += 4; return v }
  const f32 = () => { const v = dv.getFloat32(at, true); at += 4; return v }
  if (u32() !== 0x3257_5243) throw Error("bad magic")
  if (u16() !== 2) throw Error("bad version")
  u16()
  const seq = dv.getBigUint64(at, true); at += 8
  const nStrings = u32(), nStyles = u32(), nSpans = u32()
  const strings: string[] = []
  const td = new TextDecoder()
  for (let i = 0; i < nStrings; i++) {
    const len = u32()
    strings.push(td.decode(buf.subarray(at, at + len)))
    at += len
  }
  for (let i = 0; i < nStyles; i++) skipStyle()
  const spans = []
  for (let i = 0; i < nSpans; i++) {
    const start = u32(), fontSize = f32(), color = u32(), weight = u16(), flags = u8()
    u8()
    spans.push({ start, fontSize, color, weight, italic: !!(flags & 1) })
  }
  const ops: Op[] = []
  while (at < buf.byteLength) {
    const tag = u8()
    const id = tag === 0x02 ? 0 : u32()
    const op: Op = { tag, id, f: [] }
    switch (tag) {
      case 0x01: op.f.push(u8()); break // create kind
      case 0x02: op.f.push(u32(), u32(), u32()); op.id = op.f[1]!; break // place
      case 0x03: case 0x04: break // detach, remove
      case 0x10: op.f.push(u32()); break // layout style ref
      case 0x20: { // spatial
        const m = u8(); op.f.push(m)
        if (m & 1) for (let i = 0; i < 6; i++) op.f.push(f32())
        if (m & 2) op.f.push(f32())
        break
      }
      case 0x30: { // paint
        const m = u8(); op.f.push(m)
        if (m & 1) op.f.push(u32())
        if (m & 2) op.f.push(f32())
        if (m & 4) op.f.push(u32(), f32())
        break
      }
      case 0x40: op.s = strings[u32()]; op.f.push(u32(), u32()); break // paragraph
      case 0x41: op.f.push(f32(), u32()); op.s = strings[u32()]; op.f.push(u8()); break
      case 0x50: op.f.push(u8()); break // role
      case 0x51: op.s = strings[u32()]; break // label
      case 0x60: op.f.push(u32(), u8()); break // interaction
      case 0x70: op.f.push(u32(), u32(), u32(), u32(), u32()); break // surface
      case 0x71: { const n = u32(); op.bytes = buf.slice(at, at + n); at += n; break }
      case 0x80: { // command
        const c = u8(); op.f.push(c)
        if (c === 2) op.s = strings[u32()]
        else if (c === 3) op.f.push(f32(), f32())
        break
      }
      default: throw Error(`unknown op 0x${tag.toString(16)}`)
    }
    ops.push(op)
  }
  return { seq, strings, styleCount: nStyles, spans, ops }

  function skipStyle() {
    const mask = dv.getBigUint64(at, true); at += 8
    const lp = () => { const t = u8(); if (t < 2) at += 4 }
    const dim = () => { const t = u8(); if (t <= 1 || t === 5 || t === 6) at += 4 }
    for (let bit = 0; bit < 21; bit++) {
      if (!(mask & (1n << BigInt(bit)))) continue
      if (bit <= 7) at += 1
      else if (bit === 8 || bit === 10 || bit === 11) { lp(); lp() }
      else if (bit === 9) { dim(); dim() }
      else if (bit >= 12 && bit <= 15) { for (let i = 0; i < 4; i++) lp() }
      else if (bit === 16) dim()
      else if (bit <= 19) at += 4
      else at += 2
    }
  }
}
