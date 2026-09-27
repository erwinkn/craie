// E19 app (bench/e19.sh): a 40×40 marker at the top-left that the native
// probe (crates/platform-winit/src/probe.rs) clicks. The app answers the
// n-th click by filling the marker with `n << 8 | 0xff`, under the loads
// `E19_LOAD` names (comma-separated; default "idle", none):
//
// - stream: a 400-message thread whose newest reply streams 60 tokens a
//   second, re-parsed as light markdown (paragraphs, **bold**, `code`) on
//   every token, as a chat client does.
// - gc: about 150 MB retained and allocation churn, so the collector
//   scavenges every few frames and collects fully every few seconds.
//
// Each click's handler and commit, and each GC pause, are stamped on
// `process.hrtime` (the probe's clock). On exit they go next to the
// probe's CSV: `<base>-js.csv` (seq,handler,committed) and
// `<base>-gc.csv` (start,duration,kind), all in ns, and the native
// frame statistics, `<base>-frames.csv` (fps,cpuMs,maxCpuMs).

import React, { memo, useEffect, useLayoutEffect, useState } from "react"
import { writeFileSync } from "node:fs"
import { PerformanceObserver, constants, performance } from "node:perf_hooks"
import { attachApp, onFrameStats, ScrollView, Text, View } from "@craie/react"

const LOAD = new Set((process.env.E19_LOAD ?? "idle").split(","))
const hr = () => Number(process.hrtime.bigint())

const BG = "#0a0c11"
const PANEL = "#151924"
const FG = "#e9ecf4"
const DIM = "#8a93a8"
const CODE = "#5ee6ff"

// -------------------------------------------------------------- stamps

/** Per click (index = seq, 0 unused): its handler ran, its commit ran. */
const handled: number[] = [0]
const committed: number[] = [0]
const pauses: string[] = []
const frames: string[] = []
onFrameStats((s) => frames.push(`${s.fps},${s.cpuMs},${s.maxCpuMs}`))

// performance.now() is hrtime since the time origin: one offset joins them.
const origin = hr() - performance.now() * 1e6
const GC_KIND: Record<number, string> = {
  [constants.NODE_PERFORMANCE_GC_MINOR]: "minor",
  [constants.NODE_PERFORMANCE_GC_MAJOR]: "major",
  [constants.NODE_PERFORMANCE_GC_INCREMENTAL]: "incremental",
  [constants.NODE_PERFORMANCE_GC_WEAKCB]: "weak",
}
new PerformanceObserver((list) => {
  for (const e of list.getEntries()) {
    const kind = GC_KIND[(e.detail as { kind: number }).kind] ?? "other"
    pauses.push(`${Math.round(origin + e.startTime * 1e6)},${Math.round(e.duration * 1e6)},${kind}`)
  }
}).observe({ entryTypes: ["gc"] })

const base = (process.env.CRAIE_E19 ?? "e19.csv").replace(/\.csv$/, "")
process.on("exit", () => {
  const js = handled.slice(1).map((h, i) => `${i + 1},${h},${committed[i + 1] ?? 0}`)
  writeFileSync(`${base}-js.csv`, ["seq,handler,committed", ...js].join("\n") + "\n")
  writeFileSync(`${base}-gc.csv`, ["start,duration,kind", ...pauses].join("\n") + "\n")
  writeFileSync(`${base}-frames.csv`, ["fps,cpuMs,maxCpuMs", ...frames].join("\n") + "\n")
})

// -------------------------------------------------------------- marker

function Marker() {
  const [n, setN] = useState(0)
  useLayoutEffect(() => {
    const now = hr()
    while (committed.length <= n) committed.push(now)
  }, [n])
  return (
    <View
      style={{ width: 40, height: 40 }}
      backgroundColor={((n << 8) | 0xff) >>> 0}
      onPointerDown={() => {
        handled.push(hr())
        setN(handled.length - 1)
      }}
    />
  )
}

// -------------------------------------------------------------- stream

const WORDS = (
  "the native frame commits a layout while React renders each reply into " +
  "spans and the worker drains events from the platform loop so every " +
  "token lands in its paragraph with glyph runs cached across frames"
).split(" ")
let seed = 0xe19
const rand = () => (seed = (Math.imul(seed, 1103515245) + 12345) >>> 0) / 2 ** 32
function token(): string {
  const w = WORDS[Math.floor(rand() * WORDS.length)]!
  const r = rand()
  return r < 0.03 ? `\n\n${w}` : r < 0.08 ? ` **${w}**` : r < 0.12 ? ` \`${w}\`` : ` ${w}`
}
const message = (tokens: number) => Array.from({ length: tokens }, token).join("").trim()

const THREAD = 400
const TOKENS_PER_S = 60
const REPLY_TOKENS = 600

interface Msg { id: number; text: string }

function Markdown({ text }: { text: string }) {
  return text.split("\n\n").map((para, i) => (
    <Text key={i} fontSize={14} lineHeight={20} color={FG}>
      {para.split(/(\*\*[^*]+\*\*|`[^`]+`)/).map((s, j) =>
        s.startsWith("**") ? <Text key={j} fontWeight="bold">{s.slice(2, -2)}</Text>
        : s.startsWith("`") ? <Text key={j} fontFamily="monospace" color={CODE}>{s.slice(1, -1)}</Text>
        : <Text key={j}>{s}</Text>,
      )}
    </Text>
  ))
}

const Message = memo(function Message({ text }: { text: string }) {
  return (
    <View backgroundColor={PANEL} borderRadius={8} style={{ padding: 12, gap: 8 }}>
      <Markdown text={text} />
    </View>
  )
})

/** Newest first, so the streaming reply is on screen without scrolling. */
function Thread() {
  const [thread, setThread] = useState<Msg[]>(() =>
    Array.from({ length: THREAD }, (_, id) => ({ id, text: message(20 + ((id * 37) % 100)) })),
  )
  const [reply, setReply] = useState("")
  useEffect(() => {
    let text = ""
    let count = 0
    let next = THREAD
    const start = performance.now()
    const id = setInterval(() => {
      const due = Math.floor(((performance.now() - start) * TOKENS_PER_S) / 1000)
      for (; count < due; count++) {
        text += token()
        if ((count + 1) % REPLY_TOKENS === 0) {
          const done = { id: next++, text: text.trim() }
          setThread((t) => [...t.slice(1), done])
          text = ""
        }
      }
      setReply(text.trim())
    }, 1000 / TOKENS_PER_S)
    return () => clearInterval(id)
  }, [])
  return (
    <ScrollView style={{ flexGrow: 1, flexBasis: 0 }}>
      <View style={{ padding: 16, gap: 12 }}>
        <Message text={reply} />
        {thread.toReversed().map((m) => <Message key={m.id} text={m.text} />)}
      </View>
    </ScrollView>
  )
}

// ------------------------------------------------------------------ gc

/** About 150 MB of records, 1/400 of them replaced every 10 ms (the set
 * turns over every 4 s, so old space fills and is collected), plus 1 MB
 * of short-lived garbage per tick for the scavenger. */
function churn() {
  const N = 800_000
  const record = (i: number) => ({ id: i, name: `record-${i}-${rand()}`, tags: [i, i * 2, i * 3], at: 0 })
  const keep = Array.from({ length: N }, (_, i) => record(i))
  let at = 0
  let sink = 0
  setInterval(() => {
    for (let i = 0; i < 5_000; i++) sink += record(i).name.length
    for (let i = 0; i < N / 400; i++, at = (at + 1) % N) keep[at] = record(at)
    if (sink < 0) console.log(sink)
  }, 10)
  console.log(`[e19] gc load: ${Math.round(process.memoryUsage().heapUsed / 2 ** 20)} MB heap`)
}

// ----------------------------------------------------------------- app

function App() {
  return (
    <View backgroundColor={BG} style={{ width: "100%", height: "100%" }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 12 }}>
        <Marker />
        <Text fontSize={13} color={DIM}>{`E19 · load: ${[...LOAD].join(", ")}`}</Text>
      </View>
      {LOAD.has("stream") && <Thread />}
    </View>
  )
}

if (LOAD.has("gc")) churn()
const root = attachApp()
root.render(<App />)
