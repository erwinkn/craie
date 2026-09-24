// Pulse: a live operations wall in React on Craie.
//
// - A heat wall of up to 10,000 tiles. Heat drifts through native color
//   transitions; the pointer brushes tiles and a click sends a ripple
//   through all of them. Every pop is two native tweens: JS starts them,
//   native runs every frame.
// - A virtualized log of 200,000 entries with styled spans, streaming,
//   filterable, selectable.
// - 24 sparklines (native Bars surfaces fed by typed arrays) at 20 Hz.
// - A HUD with the native frame loop's own statistics.
//
//   pnpm dev:pulse

import React, { memo, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react"
import { readFileSync } from "node:fs"
import {
  attachApp,
  Bars,
  List,
  Pressable,
  ScrollView,
  Text,
  TextInput,
  Vector,
  View,
  useFrameStats,
  onFrameStats,
  type HostNode,
  type PointerEvt,
} from "@craie/react"

// ------------------------------------------------------------- palette

const BG = "#0a0c11"
const PANEL = "#11141c"
const RAISED = "#171b26"
const EDGE = "#222838"
const FG = "#e9ecf4"
const DIM = "#8a93a8"
const FAINT = "#566076"
const CYAN = "#5ee6ff"
const VIOLET = "#8a7dff"
const PINK = "#ff5cae"
const LIME = "#a6ff5c"
const AMBER = "#ffb547"

const icon = (name: string) =>
  new Uint8Array(readFileSync(new URL(`./icons/${name}.crv`, import.meta.url)))
const ICONS = {
  logo: icon("logo"),
  ripple: icon("ripple"),
  bolt: icon("bolt"),
  stream: icon("stream"),
  flame: icon("flame"),
}

/** The heat scale: 32 steps from deep blue through cyan and amber to
 * pink. */
const HEAT_STEPS = 32
const HEAT: string[] = (() => {
  const stops: [number, number, number, number][] = [
    [0.0, 0x13, 0x1d, 0x33],
    [0.3, 0x1b, 0x5f, 0xa0],
    [0.55, 0x5e, 0xe6, 0xff],
    [0.75, 0xff, 0xb5, 0x47],
    [1.0, 0xff, 0x5c, 0xae],
  ]
  const out: string[] = []
  for (let i = 0; i < HEAT_STEPS; i++) {
    const t = i / (HEAT_STEPS - 1)
    let k = 0
    while (k < stops.length - 2 && t > stops[k + 1]![0]) k++
    const [t0, r0, g0, b0] = stops[k]!
    const [t1, r1, g1, b1] = stops[k + 1]!
    const u = (t - t0) / (t1 - t0)
    const c = (a: number, b: number) =>
      Math.round(a + (b - a) * u).toString(16).padStart(2, "0")
    out.push(`#${c(r0, r1)}${c(g0, g1)}${c(b0, b1)}`)
  }
  return out
})()

// ------------------------------------------------------ shared workload

/** xorshift32 in [0, 1): the same sequences as the GPUI version
 * (`bench/gpui-pulse`), so both do the same work. */
function seeded(seed: number): () => number {
  let x = seed >>> 0
  return () => {
    x ^= x << 13
    x >>>= 0
    x ^= x >>> 17
    x ^= x << 5
    x >>>= 0
    return x / 4294967296
  }
}
const heatRng = seeded(1)
const stormRng = seeded(2)
const streamRng = seeded(3)
const sparkRng = seeded(4)

/** Work done, counted for PULSE_LOG. */
const counts = { heat: 0, entries: 0, sparks: 0, ripples: 0 }

/** Runs `f` every `ms`, catching up (up to eight runs) when late, as the
 * GPUI version's timers do. Returns the stop function. */
function every(ms: number, f: () => void): () => void {
  const start = performance.now()
  let done = 0
  const id = setInterval(() => {
    const due = Math.floor((performance.now() - start) / ms)
    const n = Math.min(due - done, 8)
    done = due
    for (let k = 0; k < n; k++) f()
  }, ms)
  return () => clearInterval(id)
}

// ------------------------------------------------------------ heat wall

const FIELD_W = 956
const FIELD_H = 600
const MAX_TILES = 10_000
const COUNTS = [2_500, 5_000, 10_000] as const

interface Grid {
  count: number
  cols: number
  rows: number
  pitch: number
  side: number
  gap: number
}

function grid(count: number): Grid {
  const cols = Math.ceil(Math.sqrt((count * FIELD_W) / FIELD_H))
  const rows = Math.ceil(count / cols)
  const pitch = Math.floor(Math.min(FIELD_W / cols, FIELD_H / rows) * 4) / 4
  const gap = Math.max(1, Math.round(pitch * 0.16))
  return { count, cols, rows, pitch, side: pitch - gap, gap }
}

/** Per-tile heat (0..HEAT_STEPS-1) with one listener per tile, so a heat
 * tick re-renders only the tiles it changes. */
const heat = new Uint8Array(MAX_TILES)
const heatListeners: ((() => void) | undefined)[] = new Array(MAX_TILES)
function useHeat(i: number): number {
  return useSyncExternalStore(
    (cb) => {
      heatListeners[i] = cb
      return () => {
        heatListeners[i] = undefined
      }
    },
    () => heat[i]!,
  )
}

const tileNodes: (HostNode | null)[] = new Array(MAX_TILES).fill(null)
/** Tiles with a pop running (a scale tween, then a spring back). */
const popping = new Uint8Array(MAX_TILES)

const TILE_TRANSITION = { backgroundColor: { duration: 520, easing: "ease-out" } } as const

const Tile = memo(function Tile({ i, side, radius }: { i: number; side: number; radius: number }) {
  const h = useHeat(i)
  const ref = useCallback((n: HostNode | null) => {
    tileNodes[i] = n
  }, [i])
  return (
    <View
      ref={ref}
      backgroundColor={HEAT[h]}
      borderRadius={radius}
      style={{ width: side, height: side, transition: TILE_TRANSITION }}
    />
  )
})

/** Pops tile `i`: a quick scale-up, then a spring back. Both run
 * natively; JS only starts them. */
function pop(i: number, scale: number, delay: number) {
  const n = tileNodes[i]
  if (!n || popping[i]) return
  popping[i] = 1
  n.animate("transform", [{ scale }], { duration: 110, delay, easing: "ease-out" }).then((end) => {
    if (end.reason === "removed") {
      popping[i] = 0
      return
    }
    n.animate("transform", [{ scale: 1 }], { spring: { stiffness: 320, damping: 13 } }).then(() => {
      popping[i] = 0
    })
  })
}

function ripple(g: Grid, col: number, row: number) {
  counts.ripples += 1
  for (let i = 0; i < g.count; i++) {
    const d = Math.hypot((i % g.cols) - col, Math.floor(i / g.cols) - row)
    pop(i, 1 + 0.85 * Math.exp(-d / 26), d * 11)
  }
}

function HeatWall({ g, storm }: { g: Grid; storm: boolean }) {
  const radius = g.side > 9 ? 3 : g.side > 5 ? 1.5 : 1
  const tiles = useMemo(() => {
    const out: React.ReactNode[] = []
    for (let i = 0; i < g.count; i++) out.push(<Tile key={i} i={i} side={g.side} radius={radius} />)
    return out
  }, [g, radius])
  const brush = (e: PointerEvt) => {
    const col = Math.floor((e.rx ?? -1) / g.pitch)
    const row = Math.floor((e.ry ?? -1) / g.pitch)
    const reach = g.count > 5000 ? 3 : 2
    for (let dr = -reach; dr <= reach; dr++) {
      for (let dc = -reach; dc <= reach; dc++) {
        const c = col + dc
        const r = row + dr
        if (c < 0 || r < 0 || c >= g.cols || r >= g.rows) continue
        const i = r * g.cols + c
        if (i < g.count && dr * dr + dc * dc <= reach * reach) {
          pop(i, 1.9 - 0.25 * Math.hypot(dr, dc), 0)
        }
      }
    }
  }
  useEffect(() => {
    if (!storm) return
    return every(1400, () => {
      const col = stormRng() * g.cols
      ripple(g, col, stormRng() * g.rows)
    })
  }, [g, storm])
  return (
    <View
      onPointerMove={brush}
      onPointerDown={(e) =>
        ripple(g, Math.floor((e.rx ?? 0) / g.pitch), Math.floor((e.ry ?? 0) / g.pitch))
      }
      style={{
        flexDirection: "row",
        flexWrap: "wrap",
        gap: g.gap,
        width: g.cols * g.pitch,
        height: g.rows * g.pitch,
      }}
    >
      {tiles}
    </View>
  )
}

/** Drifting weather: a few warm fronts move across the wall; each tick
 * re-samples a slice of the tiles. */
function useHeatDrift(g: Grid, on: boolean) {
  useEffect(() => {
    let t = 0
    const fronts = Array.from({ length: 4 }, (_, k) => ({
      x: heatRng(),
      y: heatRng(),
      vx: (heatRng() - 0.5) * 0.02,
      vy: (heatRng() - 0.5) * 0.02,
      r: 0.12 + 0.08 * k,
    }))
    const sample = (i: number) => {
      const x = (i % g.cols) / g.cols
      const y = Math.floor(i / g.cols) / g.rows
      let v = 0.08
      for (const f of fronts) {
        const d2 = (x - f.x) ** 2 + (y - f.y) ** 2
        v += Math.exp(-d2 / (f.r * f.r))
      }
      v += 0.08 * Math.sin(x * 23 + t * 0.7) * Math.cos(y * 17 - t * 0.5)
      return Math.max(0, Math.min(HEAT_STEPS - 1, Math.round(v * 0.62 * (HEAT_STEPS - 1))))
    }
    for (let i = 0; i < g.count; i++) heat[i] = sample(i)
    for (let i = 0; i < g.count; i++) heatListeners[i]?.()
    if (!on) return
    const slice = Math.ceil(g.count / 10)
    let cursor = 0
    return every(70, () => {
      counts.heat += 1
      t += 1
      for (const f of fronts) {
        f.x += f.vx
        f.y += f.vy
        if (f.x < 0 || f.x > 1) f.vx = -f.vx
        if (f.y < 0 || f.y > 1) f.vy = -f.vy
      }
      // A strided slice, so change spreads evenly over the wall.
      for (let k = 0; k < slice; k++) {
        const i = (cursor + k * 10) % g.count
        const v = sample(i)
        if (v !== heat[i]) {
          heat[i] = v
          heatListeners[i]?.()
        }
      }
      cursor = (cursor + 1) % 10
    })
  }, [g, on])
}

// ----------------------------------------------------------------- log

interface LogEntry {
  id: number
  time: string
  level: 0 | 1 | 2 | 3
  service: string
  message: string
}

const LEVELS = [
  { name: "DEBUG", color: FAINT },
  { name: "INFO", color: CYAN },
  { name: "WARN", color: AMBER },
  { name: "ERROR", color: PINK },
] as const
const SERVICES = ["edge", "auth", "billing", "search", "render", "queue", "ledger", "gateway"]
const VERBS = ["accepted", "routed", "retried", "cached", "flushed", "rebalanced", "sealed", "streamed", "throttled", "resolved"]
const NOUNS = ["request", "session", "shard", "batch", "token", "invoice", "frame", "cursor", "lease", "snapshot"]

function entry(id: number): LogEntry {
  // Deterministic and cheap: 200,000 of these build in a few ms.
  const h = Math.imul(id ^ 0x9e3779b9, 0x85ebca6b) >>> 0
  const level = (h % 100 < 55 ? 1 : h % 100 < 80 ? 0 : h % 100 < 94 ? 2 : 3) as LogEntry["level"]
  const ms = 36_000_000 + id * 137
  const hh = Math.floor(ms / 3_600_000) % 24
  const mm = Math.floor(ms / 60_000) % 60
  const ss = Math.floor(ms / 1000) % 60
  const pad = (v: number, n = 2) => String(v).padStart(n, "0")
  const extra = h % 7 === 0 ? ` after ${1 + (h % 900)} ms; upstream ${SERVICES[(h >>> 7) % SERVICES.length]} reported ${h % 50} pending` : ""
  return {
    id,
    time: `${pad(hh)}:${pad(mm)}:${pad(ss)}.${pad(ms % 1000, 3)}`,
    level,
    service: SERVICES[(h >>> 3) % SERVICES.length]!,
    message: `${NOUNS[(h >>> 11) % NOUNS.length]} ${(h >>> 5) % 99999} ${VERBS[(h >>> 17) % VERBS.length]}${extra}`,
  }
}

const LOG_FONT = 12.5
const LogRow = memo(function LogRow({ e }: { e: LogEntry }) {
  const level = LEVELS[e.level]
  return (
    <View style={{ padding: { left: 14, right: 14, top: 3, bottom: 3 } }}>
      <Text fontSize={LOG_FONT} color={FG} lineHeight={18}>
        <Text fontFamily="Menlo" color={FAINT}>{e.time}  </Text>
        <Text fontFamily="Menlo" fontWeight="bold" color={level.color}>
          {level.name.padEnd(5)}
        </Text>
        <Text color={VIOLET}>{`  ${e.service}  `}</Text>
        {e.message}
      </Text>
    </View>
  )
})

function LogPanel({ streaming }: { streaming: boolean }) {
  const [entries, setEntries] = useState<LogEntry[]>(() =>
    Array.from({ length: 200_000 }, (_, i) => entry(i)),
  )
  const [query, setQuery] = useState("")
  const scroller = useRef<HostNode>(null)
  useEffect(() => {
    scroller.current?.scrollTo(0, 1e9)
  }, [])
  useEffect(() => {
    if (!streaming) return
    return every(60, () => {
      const n = 1 + Math.min(3, Math.floor(streamRng() * 4))
      counts.entries += n
      setEntries((es) => {
        const next = es.slice()
        for (let k = 0; k < n; k++) next.push(entry(next.length))
        return next
      })
    })
  }, [streaming])
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return entries
    return entries.filter(
      (e) =>
        e.service.includes(q) ||
        LEVELS[e.level].name.toLowerCase() === q ||
        e.message.includes(q),
    )
  }, [entries, query])
  return (
    <View
      backgroundColor={PANEL}
      borderRadius={14}
      borderColor={EDGE}
      borderWidth={1}
      style={{ width: 420, flexShrink: 0, overflow: "hidden" }}
    >
      <View style={{ padding: { left: 14, right: 14, top: 12, bottom: 10 }, gap: 8 }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Vector asset={ICONS.stream} style={{ width: 16, height: 16 }} />
          <Text fontSize={14} fontWeight="bold" color={FG}>Event log</Text>
          <View style={{ flexGrow: 1 }} />
          <Text fontSize={12} color={DIM}>
            {`${shown.length.toLocaleString("en-US")} of ${entries.length.toLocaleString("en-US")}`}
          </Text>
        </View>
        <TextInput
          placeholder="Filter: service, level, or text"
          onChangeText={setQuery}
          fontSize={13}
          color={FG}
          backgroundColor={RAISED}
          borderColor={EDGE}
          borderWidth={1}
          borderRadius={8}
          style={{ padding: { left: 10, right: 10, top: 7, bottom: 7 } }}
        />
      </View>
      <ScrollView
        ref={scroller}
        anchor="stick-to-end"
        selectable
        style={{ flexGrow: 1, flexShrink: 1 }}
      >
        <List<LogEntry>
          items={shown}
          keyOf={(e) => e.id}
          renderItem={(e) => <LogRow e={e} />}
          templates={[{ base: 6, inset: 28, fontSize: LOG_FONT }]}
          describe={(e) => ({ template: 0, textLength: e.message.length + e.service.length + 24 })}
          overscan={300}
        />
      </ScrollView>
    </View>
  )
}

// ----------------------------------------------------------- sparklines

const SPARKS = 24
const SPARK_LEN = 64
const SPARK_COLORS = [CYAN, VIOLET, PINK, LIME, AMBER]
const SPARK_NAMES = ["p50", "p99", "rps", "err", "cpu", "mem", "io", "gc"]

function Sparklines({ on }: { on: boolean }) {
  // Lazy: built once (a `useRef(value)` argument runs on every render and
  // would draw from the seeded sequence each time).
  const [initial] = useState(() =>
    Array.from({ length: SPARKS }, (_, s) => {
      const v = new Float32Array(SPARK_LEN)
      let x = 0.5
      for (let i = 0; i < SPARK_LEN; i++) {
        x = Math.max(0.05, Math.min(1, x + (sparkRng() - 0.5) * 0.18))
        v[i] = x
      }
      return { values: v, phase: s }
    }),
  )
  const series = useRef(initial)
  const [, setTick] = useState(0)
  useEffect(() => {
    if (!on) return
    return every(50, () => {
      counts.sparks += 1
      for (const s of series.current) {
        const next = new Float32Array(SPARK_LEN)
        next.set(s.values.subarray(1))
        const last = s.values[SPARK_LEN - 1]!
        next[SPARK_LEN - 1] = Math.max(0.05, Math.min(1, last + (sparkRng() - 0.5) * 0.2))
        s.values = next
      }
      setTick((k) => k + 1)
    })
  }, [on])
  const card = (i: number) => {
    const s = series.current[i]!
    const color = SPARK_COLORS[i % SPARK_COLORS.length]!
    const last = s.values[SPARK_LEN - 1]!
    return (
      <View
        key={i}
        backgroundColor={PANEL}
        borderColor={EDGE}
        borderWidth={1}
        borderRadius={10}
        style={{
          flexGrow: 1,
          flexBasis: 0,
          flexShrink: 1,
          padding: { left: 9, right: 9, top: 6, bottom: 7 },
          gap: 4,
        }}
      >
        <View style={{ flexDirection: "row" }}>
          <Text fontSize={10.5} color={DIM}>{`${SPARK_NAMES[i % SPARK_NAMES.length]}·${Math.floor(i / 8) + 1}`}</Text>
          <View style={{ flexGrow: 1 }} />
          <Text fontSize={10.5} fontFamily="Menlo" color={color}>{(last * 100).toFixed(1)}</Text>
        </View>
        <Bars values={s.values} color={EDGE} maxColor={color} gap={1} style={{ height: 20 }} />
      </View>
    )
  }
  const rows = []
  for (let r = 0; r < SPARKS / 8; r++) {
    const cards = []
    for (let c = 0; c < 8; c++) cards.push(card(r * 8 + c))
    rows.push(
      <View key={r} style={{ flexDirection: "row", gap: 8 }}>
        {cards}
      </View>,
    )
  }
  return <View style={{ gap: 8 }}>{rows}</View>
}

// ------------------------------------------------------------- controls

function Toggle({
  label,
  asset,
  on,
  onPress,
  accent,
}: {
  label: string
  asset: Uint8Array
  on: boolean
  onPress: () => void
  accent: string
}) {
  const [hover, setHover] = useState(false)
  return (
    <Pressable
      onPress={onPress}
      onPointerEnter={() => setHover(true)}
      onPointerLeave={() => setHover(false)}
      backgroundColor={on ? RAISED : hover ? "#141824" : PANEL}
      borderColor={on ? accent : EDGE}
      borderWidth={1}
      borderRadius={9}
      accessibilityLabel={label}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: 7,
        padding: { left: 10, right: 12, top: 6, bottom: 6 },
        transition: {
          backgroundColor: { duration: 160 },
          borderColor: { duration: 160 },
          transform: { spring: { stiffness: 500, damping: 22 } },
        },
        transform: [{ scale: hover ? 1.04 : 1 }],
      }}
    >
      <Vector asset={asset} style={{ width: 15, height: 15 }} />
      <Text fontSize={12.5} color={on ? FG : DIM}>{label}</Text>
    </Pressable>
  )
}

function Segmented({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  return (
    <View
      backgroundColor={PANEL}
      borderColor={EDGE}
      borderWidth={1}
      borderRadius={9}
      style={{ flexDirection: "row", padding: 3, gap: 2 }}
    >
      {COUNTS.map((c) => (
        <Pressable
          key={c}
          onPress={() => onChange(c)}
          backgroundColor={c === value ? RAISED : PANEL}
          borderRadius={7}
          accessibilityLabel={`${c} tiles`}
          style={{
            padding: { left: 10, right: 10, top: 4, bottom: 4 },
            transition: { backgroundColor: { duration: 160 } },
          }}
        >
          <Text fontSize={12} color={c === value ? CYAN : DIM}>{c.toLocaleString("en-US")}</Text>
        </Pressable>
      ))}
    </View>
  )
}

// ------------------------------------------------------------------ HUD

function Stat({ label, value, color }: { label: string; value: string; color: string }) {
  return (
    <View
      backgroundColor={PANEL}
      borderColor={EDGE}
      borderWidth={1}
      borderRadius={9}
      style={{ padding: { left: 10, right: 10, top: 5, bottom: 6 }, minWidth: 92 }}
    >
      <Text fontSize={10} color={FAINT}>{label}</Text>
      <Text fontSize={15} fontFamily="Menlo" color={color}>{value}</Text>
    </View>
  )
}

function Hud() {
  const stats = useFrameStats()
  const [idle, setIdle] = useState(true)
  useEffect(() => {
    if (!stats) return
    setIdle(false)
    const t = setTimeout(() => setIdle(true), 1200)
    return () => clearTimeout(t)
  }, [stats])
  const live = stats && !idle
  return (
    <View style={{ flexDirection: "row", gap: 8 }}>
      <Stat label="FRAMES / S" value={live ? stats.fps.toFixed(0) : "idle"} color={live ? LIME : FAINT} />
      <Stat label="CPU / FRAME" value={live ? `${stats.cpuMs.toFixed(2)} ms` : "0 ms"} color={CYAN} />
      <Stat label="WORST FRAME" value={live ? `${stats.maxCpuMs.toFixed(2)} ms` : "—"} color={AMBER} />
      <Stat label="LAYOUT + SCENE" value={live ? `${stats.prepareMs.toFixed(2)} ms` : "—"} color={VIOLET} />
      <Stat label="NATIVE NODES" value={stats ? stats.nodes.toLocaleString("en-US") : "—"} color={FG} />
      <Stat label="NATIVE TWEENS" value={live ? stats.tweens.toLocaleString("en-US") : "0"} color={PINK} />
    </View>
  )
}

// ------------------------------------------------------------------ app

function App() {
  // Scripted runs (measurements): PULSE_TILES, PULSE_STORM.
  const [count, setCount] = useState<number>(() => Number(process.env.PULSE_TILES) || 5_000)
  const [heatOn, setHeatOn] = useState(true)
  const [storm, setStorm] = useState(() => !!process.env.PULSE_STORM)
  const [streaming, setStreaming] = useState(true)
  const g = useMemo(() => grid(count), [count])
  useHeatDrift(g, heatOn)
  return (
    <View backgroundColor={BG} style={{ width: "100%", height: "100%", padding: 16, gap: 14 }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 12 }}>
        <Vector asset={ICONS.logo} style={{ width: 40, height: 40 }} />
        <View>
          <Text fontSize={22} fontWeight="bold" color={FG} letterSpacing={0.4}>Pulse</Text>
          <Text fontSize={12} color={DIM}>React 19 → Craie · retained scene · native animation</Text>
        </View>
        <View style={{ flexGrow: 1 }} />
        <Hud />
      </View>
      {/* minHeight 0: the log's content (200,000 rows) must not set the
          body's automatic minimum height. */}
      <View style={{ flexDirection: "row", gap: 16, flexGrow: 1, flexShrink: 1, minHeight: 0 }}>
        <View style={{ flexGrow: 1, flexShrink: 1, gap: 14, minHeight: 0 }}>
          <View
            backgroundColor={PANEL}
            borderColor={EDGE}
            borderWidth={1}
            borderRadius={14}
            style={{ flexGrow: 1, flexShrink: 1, overflow: "hidden" }}
          >
            <View
              style={{
                flexDirection: "row",
                alignItems: "center",
                gap: 8,
                padding: { left: 14, right: 12, top: 10, bottom: 10 },
              }}
            >
              <Text fontSize={14} fontWeight="bold" color={FG}>Heat wall</Text>
              <Text fontSize={12} color={DIM}>move to brush · click to ripple</Text>
              <View style={{ flexGrow: 1 }} />
              <Segmented value={count} onChange={setCount} />
              <Toggle label="Heat" asset={ICONS.flame} on={heatOn} accent={PINK} onPress={() => setHeatOn((v) => !v)} />
              <Toggle label="Storm" asset={ICONS.bolt} on={storm} accent={AMBER} onPress={() => setStorm((v) => !v)} />
              <Toggle label="Stream" asset={ICONS.stream} on={streaming} accent={LIME} onPress={() => setStreaming((v) => !v)} />
              <Toggle label="Ripple" asset={ICONS.ripple} on={false} accent={CYAN} onPress={() => ripple(g, g.cols / 2, g.rows / 2)} />
            </View>
            <View style={{ flexGrow: 1, alignItems: "center", justifyContent: "center" }}>
              <HeatWall key={count} g={g} storm={storm} />
            </View>
          </View>
          <Sparklines on={streaming} />
        </View>
        <LogPanel streaming={streaming} />
      </View>
    </View>
  )
}

const root = attachApp()
const t0 = performance.now()
root.render(<App />)
console.log("[pulse] attached")
// PULSE_LOG: native frame statistics on stdout, with this process's CPU
// (main thread, JS worker, and every other thread) and the work done per
// second (measurements).
if (process.env.PULSE_LOG) {
  let at = performance.now()
  let cpu = process.cpuUsage()
  onFrameStats((s) => {
    const now = performance.now()
    const secs = (now - at) / 1000
    const used = process.cpuUsage(cpu)
    const pct = ((used.user + used.system) / 1e6 / secs) * 100
    const total = process.cpuUsage()
    const totalMs = (total.user + total.system) / 1000
    const per = (k: keyof typeof counts) => {
      const v = counts[k] / secs
      counts[k] = 0
      return v
    }
    console.log(
      `[pulse] ${(now - t0).toFixed(0)} ms: ${s.fps.toFixed(0)} fps, cpu ${s.cpuMs.toFixed(2)} ms (max ${s.maxCpuMs.toFixed(2)}), prepare ${s.prepareMs.toFixed(2)} ms, ${s.nodes} nodes, ${s.tweens} tweens; process cpu ${pct.toFixed(0)}% (total ${totalMs.toFixed(0)} ms at epoch ${Date.now()}); per s: heat ${per("heat").toFixed(1)}, entries ${per("entries").toFixed(1)}, sparks ${per("sparks").toFixed(1)}, ripples ${per("ripples").toFixed(2)}`,
    )
    at = now
    cpu = process.cpuUsage()
  })
}
