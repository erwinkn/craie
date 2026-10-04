import { test, expect } from "bun:test"
import { decodeEvents } from "../src/native.js"
import { EVENT_KIND, LIST_SLOT, NIL, decodeListAsk, decodeListViewport } from "../src/wire.js"

/** A LIST_VIEWPORT payload laid out as list.rs `ListViewport::bytes`. */
function viewportBytes(): Uint8Array {
  const b = new Uint8Array(43 + 8)
  const v = new DataView(b.buffer)
  ;[3, 20, 5, 12, 6, 8, 77, 6].forEach((x, k) => v.setUint32(4 * k, x, true))
  v.setFloat32(32, -12.5, true)
  v.setFloat32(36, 230, true)
  b[40] = 3
  v.setUint16(41, 2, true)
  v.setUint32(43, 9, true)
  v.setUint32(47, 10, true)
  return b
}

/** One outbox frame of one record (events.rs `encode_events`). */
function frame(kind: number, node: number, key: number, payload: Uint8Array): Uint8Array {
  const b = new Uint8Array(4 + 36 + payload.length)
  const v = new DataView(b.buffer)
  v.setUint32(0, 1, true)
  b[4] = kind
  v.setUint32(8, node, true)
  v.setUint32(28, key, true)
  v.setUint32(36, payload.length, true)
  b.set(payload, 40)
  return b
}

test("a list viewport report decodes", () => {
  expect(decodeListViewport(viewportBytes())).toEqual({
    mounted: { first: 3, end: 20 },
    visible: { first: 5, end: 12 },
    held: { first: 6, end: 8 },
    anchor: { item: 77, index: 6, offset: -12.5 },
    offset: 230,
    atEnd: true,
    following: true,
    pinned: [9, 10],
  })
  const none = viewportBytes()
  new DataView(none.buffer).setUint32(24, NIL, true)
  expect(decodeListViewport(none).anchor).toBeNull()
})

test("an updateItems ask decodes: load, then the unload ranges above and below", () => {
  const b = new Uint8Array(1 + 24)
  const v = new DataView(b.buffer)
  b[0] = 0b111
  ;[100, 129, 0, 9, 400, 499].forEach((x, k) => v.setUint32(1 + 4 * k, x, true))
  expect(decodeListAsk(b)).toEqual({
    load: { first: 100, last: 129 },
    unload: [{ first: 0, last: 9 }, { first: 400, last: 499 }],
  })
  expect(decodeListAsk(new Uint8Array([1, 5, 0, 0, 0, 19, 0, 0, 0]))).toEqual({ load: { first: 5, last: 19 } })
})

test("decodeEvents keeps call and listViewport bytes raw", () => {
  const ask = new Uint8Array([1, 5, 0, 0, 0, 19, 0, 0, 0])
  const [call] = decodeEvents(frame(EVENT_KIND.call, 4, LIST_SLOT.updateItems, ask))
  expect(call!.bytes).toEqual(ask)
  expect(call!.text).toBe("")
  expect(call!.key & 0xff).toBe(LIST_SLOT.updateItems)
  const [report] = decodeEvents(frame(EVENT_KIND.listViewport, 4, 0, viewportBytes()))
  expect(decodeListViewport(report!.bytes!).visible).toEqual({ first: 5, end: 12 })
})
