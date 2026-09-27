// Minimal CRW2 reader for tests: header, tables, and op records.

export interface Op {
  tag: number
  id: number
  /** Op-specific fields, in wire order. */
  f: number[]
  /** String operand (text, label, placeholder, set-text command). */
  s?: string
  bytes?: Uint8Array
  /** CLAIMS: the claim set (`f` holds the version). */
  claims?: { kind: number; flags: number; mods: number; key: number }[]
  /** VARIANTS: each variant's terms, env, and values in wire order (a
   * layout value contributes its layout keys). */
  variants?: { terms: { scope: number; mask: bigint }[]; env: number; values: number[] }[]
  /** STATES: the bits. */
  bits?: bigint
  /** DRAWING: each shape's strings (geometry, transform, dashes) and
   * numbers in wire order; `s` holds the view box. */
  shapes?: { strings: string[]; f: number[] }[]
}

export interface Frame {
  seq: bigint
  strings: string[]
  styleCount: number
  spans: {
    start: number; fontSize: number; color: number; weight: number; italic: boolean
    decoration: number; family: string | undefined; letterSpacing: number; lineHeight: number
    inheritColor: boolean
  }[]
  ops: Op[]
}

export function readFrame(buf: Uint8Array): Frame {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  let at = 0
  const u8 = () => buf[at++]!
  const u16 = () => { const v = dv.getUint16(at, true); at += 2; return v }
  const u32 = () => { const v = dv.getUint32(at, true); at += 4; return v }
  const f32 = () => { const v = dv.getFloat32(at, true); at += 4; return v }
  const u64 = () => { const v = dv.getBigUint64(at, true); at += 8; return v }
  if (u32() !== 0x3257_5243) throw Error("bad magic")
  if (u16() !== 4) throw Error("bad version")
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
    const familyRef = u32(), letterSpacing = f32(), lineHeight = f32()
    spans.push({
      start, fontSize, color, weight, italic: !!(flags & 1),
      decoration: (flags >> 1) & 3,
      family: familyRef === 0xffff_ffff ? undefined : strings[familyRef],
      letterSpacing, lineHeight, inheritColor: !!(flags & 8),
    })
  }
  const ops: Op[] = []
  while (at < buf.byteLength) {
    const tag = u8()
    const id = tag === 0x02 || tag === 0xb2 ? 0 : u32() // place, environment: no id
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
        if (m & 4) op.f.push(u32() | 0)
        break
      }
      case 0x22: op.f.push(u32()); break // layer owner
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
      case 0x61: { // claims: version, count x (kind, flags, mods, 0, key)
        op.f.push(u32())
        const n = u16()
        op.claims = []
        for (let i = 0; i < n; i++) {
          const kind = u8(), flags = u8(), mods = u8()
          u8()
          op.claims.push({ kind, flags, mods, key: u32() })
        }
        break
      }
      case 0x70: op.f.push(u32(), u32(), u32(), u32(), u32()); break // surface
      case 0x71: { const n = u32(); op.bytes = buf.slice(at, at + n); at += n; break }
      case 0x72: { // drawing: view box, count x 44-byte shapes
        op.s = strings[u32()]
        const n = u16()
        op.shapes = []
        for (let i = 0; i < n; i++) {
          const f = [u8(), u8(), u8(), u8()]
          const strs = [strings[u32()]!, strings[u32()]!, strings[u32()]!]
          f.push(u32(), u32(), f32(), f32(), f32(), f32())
          op.shapes.push({ strings: strs, f })
        }
        break
      }
      case 0x80: { // command
        const c = u8(); op.f.push(c)
        if (c === 2 || c === 4 || c === 5) op.s = strings[u32()]
        else if (c === 3) op.f.push(f32(), f32())
        break
      }
      case 0x90: { // list config: overscan, fallback, templates
        op.f.push(f32(), f32())
        const n = u16()
        for (let i = 0; i < n; i++) op.f.push(f32(), f32(), f32())
        break
      }
      case 0x91: { // list splice: at, remove, count, descs
        op.f.push(u32(), u32())
        const n = u32(); op.f.push(n)
        for (let i = 0; i < n; i++) op.f.push(u16(), u32(), u32(), u8())
        break
      }
      case 0x92: op.f.push(u32()); break // list index
      case 0x93: op.f.push(u8()); break // scroll anchor
      case 0xa0: { // transition: count x (prop, timing)
        const n = u8(); op.f.push(n)
        for (let i = 0; i < n; i++) { op.f.push(u8(), u8()); for (let k = 0; k < 6; k++) op.f.push(f32()) }
        break
      }
      case 0xa1: { // animate: prop, value by prop, timing
        const p = u8(); op.f.push(p)
        const n = [6, 1, 0, 0, 1, 1, 4, 2, 0][p]!
        if (p === 2 || p === 3 || p === 8) op.f.push(u32())
        for (let i = 0; i < n; i++) op.f.push(f32())
        op.f.push(u8()); for (let k = 0; k < 6; k++) op.f.push(f32())
        break
      }
      case 0xb0: op.bits = u64(); break // states
      case 0xb1: { // variants: count x (term count, env, terms, values)
        const n = u16()
        op.variants = []
        for (let i = 0; i < n; i++) {
          const nTerms = u8(), env = u8()
          const terms = []
          for (let k = 0; k < nTerms; k++) terms.push({ scope: u32(), mask: u64() })
          const m = u8(), values = [m]
          if (m & 1) values.push(u32())
          if (m & 2) values.push(u32())
          if (m & 4) values.push(f32())
          if (m & 8) values.push(u8(), u32())
          if (m & 16) values.push(f32())
          if (m & 32) for (let k = 0; k < 6; k++) values.push(f32())
          if (m & 64) { const keys = u64(); values.push(Number(keys)); skipFields(keyFields(keys)) }
          if (m & 128) values.push(f32())
          op.variants.push({ terms, env, values })
        }
        break
      }
      case 0xb2: op.f.push(f32(), f32()); break // environment
      case 0xb3: op.f.push(u8(), u32()); break // color
      default: throw Error(`unknown op 0x${tag.toString(16)}`)
    }
    ops.push(op)
  }
  return { seq, strings, styleCount: nStyles, spans, ops }

  function skipStyle() {
    const mask = dv.getBigUint64(at, true); at += 8
    skipFields(mask)
  }
  function skipFields(mask: bigint) {
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

/** The style fields carrying layout keys (states.rs `layout_key::fields`):
 * the first eight keys are their fields; then gap 2, size 2, min 2,
 * max 2, padding 4, margin 4, border 4, inset 4, then one each. */
function keyFields(keys: bigint): bigint {
  let fields = keys & 0xffn
  const widths = [2, 2, 2, 2, 4, 4, 4, 4, 1, 1, 1, 1, 2]
  let at = 8n
  widths.forEach((w, i) => {
    if ((keys >> at) & ((1n << BigInt(w)) - 1n)) fields |= 1n << BigInt(8 + i)
    at += BigInt(w)
  })
  return fields
}
