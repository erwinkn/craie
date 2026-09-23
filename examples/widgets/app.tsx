// Widgets app: an external behavior (Slider — pure React over pointer
// events) and a native surface (sparkline — a Bars surface fed by a
// Float32Array payload). Neither needed a second UI framework: the
// slider composes Views; the sparkline is one retained node whose data
// crosses the bridge as typed-array bytes, with no JS at paint time.

import React, { useEffect, useRef, useState } from "react"
import { attachApp, Bars, Text, View, type PointerEvt } from "@craie/react"

const BG = "#141518"
const PANEL = "#1b1d24"
const EDGE = "#2a2d38"
const FG = "#ececf0"
const DIM = "#9aa0ae"
const ACCENT = "#6dc7ff"

function Slider({
  value,
  onChange,
  width = 260,
}: {
  value: number
  onChange: (v: number) => void
  width?: number
}) {
  const dragging = useRef(false)
  // Pointer events carry `rx`: position relative to the track's border
  // box. Capture keeps move/up flowing to the track mid-drag.
  const set = (e: PointerEvt) => onChange(Math.min(1, Math.max(0, (e.rx ?? 0) / width)))
  const fillW = Math.max(0, Math.min(1, value)) * width
  return (
    <View
      backgroundColor={EDGE}
      borderRadius={4}
      onPointerDown={(e) => {
        dragging.current = true
        set(e)
      }}
      onPointerMove={(e) => {
        if (dragging.current) set(e)
      }}
      onPointerUp={() => {
        dragging.current = false
      }}
      style={{ width, height: 8 }}
    >
      <View
        backgroundColor={ACCENT}
        borderRadius={4}
        style={{ width: fillW, height: 8 }}
      />
      <View
        backgroundColor={FG}
        borderRadius={8}
        style={{
          position: "absolute",
          left: Math.max(0, fillW - 8),
          top: -4,
          width: 16,
          height: 16,
        }}
      />
    </View>
  )
}

function Labeled({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <View style={{ flexDirection: "column", gap: 8, width: "100%" }}>
      <Text fontSize={12} color={DIM}>{label}</Text>
      {children}
    </View>
  )
}

function App() {
  const [volume, setVolume] = useState(0.4)
  const [brightness, setBrightness] = useState(0.7)
  const [series, setSeries] = useState(
    () => new Float32Array([0.2, 0.5, 0.3, 0.8, 0.6, 0.9, 0.45, 0.7]),
  )
  useEffect(() => {
    const t = setInterval(
      () =>
        setSeries((s) => {
          // A new array per update: the payload compares by identity.
          const next = new Float32Array(s.length)
          next.set(s.subarray(1))
          next[s.length - 1] = Math.random() * 0.8 + 0.2
          return next
        }),
      900,
    )
    return () => clearInterval(t)
  }, [])

  return (
    <View
      backgroundColor={BG}
      style={{ flexDirection: "column", width: "100%", height: "100%", padding: 24, gap: 20 }}
    >
      <Text fontSize={22} color={FG}>widgets</Text>
      <View
        backgroundColor={PANEL}
        borderColor={EDGE}
        borderWidth={1}
        borderRadius={10}
        style={{ flexDirection: "column", padding: 16, gap: 16, width: "100%" }}
      >
        <Labeled label={`volume — ${(volume * 100).toFixed(0)}%`}>
          <Slider value={volume} onChange={setVolume} />
        </Labeled>
        <Labeled label={`brightness — ${(brightness * 100).toFixed(0)}%`}>
          <Slider value={brightness} onChange={setBrightness} />
        </Labeled>
      </View>
      <View
        backgroundColor={PANEL}
        borderColor={EDGE}
        borderWidth={1}
        borderRadius={10}
        style={{ flexDirection: "column", padding: 16, gap: 8, width: "100%" }}
      >
        <Text fontSize={12} color={DIM}>throughput — custom element</Text>
        <Bars
          values={series}
          color="#6dc7c8"
          maxColor="#6dc7ff"
          accessibilityLabel="throughput"
          style={{ width: "100%", height: 80 }}
        />
      </View>
      <Text fontSize={12} color={DIM}>
        sliders are plain Views; the chart is one native Bars surface fed
        by a Float32Array.
      </Text>
    </View>
  )
}

const root = attachApp()
root.render(<App />)
console.log("[widgets] attached")
