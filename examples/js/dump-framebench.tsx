// Writes the frame-cost benchmark transactions as the React bridge seals
// them — the craie counterpart of gpui-react's js-wire-dump.tsx.
//
//   bun examples/js/dump-framebench.tsx [dir] [rows] [ops]
//
// The scene and protocol match fixtures/performance in gpuix: an
// 800x600 document, a 32px status line, `rows` identical 20px text
// rows, then alternating status updates and scroll steps, then removal.
// Every frame here is a real React commit captured at the transport,
// so the native bench measures production wire bytes.
//
// Deliberate differences, matching Craie's current capabilities:
// - Only the `flow` scene exists. List virtualization (gpui-react's
//   `list` scene) is planned work.
// - A scroll step is a React-driven margin change on the content view —
//   what a craie app emits today. Native scrolling and input events are
//   planned; when they land, this phase becomes a native dispatch.

import React from "react"
import { createRoot, View, Text, type Transport } from "@craie/bridge"
import { mkdirSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const DIR = process.argv[2] ?? "/tmp/craie-framebench/wire"
const ROWS = Number(process.argv[3] ?? 5000)
const OPS = Number(process.argv[4] ?? 110)
const WIDTH = 800, HEIGHT = 600, HEADER = 32, ROW = 20

class Capture implements Transport {
  payloads: Uint8Array[] = []
  send(frame: Uint8Array) {
    this.payloads.push(frame.slice())
  }
  close() {}
}

const tick = () => new Promise((r) => setTimeout(r, 0))

function rowText(ix: number) {
  return `Row ${String(ix).padStart(5, "0")}: retained native content for the frame comparison`
}

function Scene({ rows, scroll, status }: { rows: number; scroll: number; status: string }) {
  const items = Array.from({ length: rows }, (_, ix) => (
    <Text
      key={ix}
      text={rowText(ix)}
      fontSize={14}
      color="#ffffff"
      style={{ width: WIDTH, height: ROW, flexShrink: 0 }}
    />
  ))
  return (
    <View
      backgroundColor="#101010"
      style={{ width: WIDTH, height: HEIGHT, flexDirection: "column" }}
    >
      <Text
        text={status}
        fontSize={14}
        color="#ffffff"
        style={{ width: WIDTH, height: HEADER, flexShrink: 0 }}
      />
      <View style={{ width: WIDTH, height: HEIGHT - HEADER, flexShrink: 0, overflow: "hidden" }}>
        {/* Scroll = translate the content view; the clip view stays put. */}
        <View style={{ width: WIDTH, flexDirection: "column", margin: { top: -scroll } }}>
          {items}
        </View>
      </View>
    </View>
  )
}

function writeFrames(path: string, frames: Uint8Array[]) {
  const buf = new Uint8Array(4 + frames.reduce((n, f) => n + 4 + f.length, 0))
  const view = new DataView(buf.buffer)
  let at = 0
  view.setUint32(at, frames.length, true)
  at += 4
  for (const f of frames) {
    view.setUint32(at, f.length, true)
    at += 4
    buf.set(f, at)
    at += f.length
  }
  writeFileSync(path, buf)
}

mkdirSync(DIR, { recursive: true })
const transport = new Capture()
const root = createRoot(transport)

/** Waits for the commit-sealing microtask and returns the one frame. */
async function take(): Promise<Uint8Array> {
  await tick()
  const frames = transport.payloads.splice(0)
  if (frames.length !== 1) throw Error(`expected 1 sealed frame, got ${frames.length}`)
  return frames[0]!
}

root.renderSync(<Scene rows={ROWS} scroll={0} status="Status 0" />)
writeFileSync(join(DIR, `mount-flow-${ROWS}.bin`), await take())

// Status line updates, alternating "Status 1"/"Status 0" per commit.
const updates: Uint8Array[] = []
for (let i = 0; i < OPS; i++) {
  root.renderSync(
    <Scene rows={ROWS} scroll={0} status={i % 2 === 0 ? "Status 1" : "Status 0"} />,
  )
  updates.push(await take())
}
writeFrames(join(DIR, `updates-flow-${ROWS}.bin`), updates)

// Scroll steps alternating one row down/up — the React-windowing
// equivalent of gpui-react's alternating wheel deltas.
const scrolls: Uint8Array[] = []
for (let i = 0; i < OPS; i++) {
  root.renderSync(
    <Scene rows={ROWS} scroll={i % 2 === 0 ? ROW : 0} status="Status 0" />,
  )
  scrolls.push(await take())
}
writeFrames(join(DIR, `scroll-flow-${ROWS}.bin`), scrolls)

root.renderSync(null)
writeFileSync(join(DIR, `remove-flow-${ROWS}.bin`), await take())

console.log(`wrote ${join(DIR, `*-flow-${ROWS}.bin`)}`)
