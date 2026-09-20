// Marbre-like demo: an agent-desktop-style chat UI rendered by React over
// the Craie wire — sidebar, message list, composer, ticking session state.
//
//   cargo run --example app &
//   bun examples/js/demo.tsx

import React, { useEffect, useState } from "react"
import { createRoot, connect, View, Text } from "@craie/bridge"

const ACCENT = "#6dc7ff"
const DIM = "#9aa0ae"
const FG = "#ececf0"

function Sidebar({ active }: { active: number }) {
  const items = ["session: craie-bench", "session: marbre-port", "session: atlas-dirty", "settings"]
  return (
    <View
      backgroundColor="#17181d"
      style={{ width: 200, padding: 12, gap: 4, height: "100%" }}
    >
      <Text fontSize={13} color={DIM}>workspace</Text>
      {items.map((s, i) => (
        <View
          key={s}
          backgroundColor={i === active ? "#262933" : "#00000000"}
          style={{ padding: { left: 8, right: 8, top: 5, bottom: 5 } }}
        >
          <Text fontSize={13} color={i === active ? FG : DIM}>{s}</Text>
        </View>
      ))}
    </View>
  )
}

function Message({ who, text, accent }: { who: string; text: string; accent?: boolean }) {
  return (
    <View
      backgroundColor={accent ? "#1d2530" : "#1b1d24"}
      style={{ padding: 12, gap: 4, width: "100%" }}
    >
      <Text fontSize={12} color={accent ? ACCENT : DIM}>{who}</Text>
      <Text fontSize={14} color={FG}>{text}</Text>
    </View>
  )
}

function Composer({ tick }: { tick: number }) {
  return (
    <View
      backgroundColor="#20222b"
      style={{
        padding: { left: 14, right: 14, top: 10, bottom: 10 },
        flexDirection: "row",
        alignItems: "center",
        gap: 10,
      }}
    >
      <Text fontSize={14} color={DIM}>{`Message agent… (uptime ${tick}s)`}</Text>
      <View
        backgroundColor={ACCENT}
        style={{ padding: { left: 10, right: 10, top: 4, bottom: 4 } }}
      >
        <Text fontSize={12} color="#141518">send</Text>
      </View>
    </View>
  )
}

function App() {
  const [tick, setTick] = useState(0)
  useEffect(() => {
    const t = setInterval(() => setTick(x => x + 1), 1000)
    return () => clearInterval(t)
  }, [])
  return (
    <View
      backgroundColor="#141518"
      style={{ flexDirection: "row", width: "100%", height: "100%" }}
    >
      <Sidebar active={tick % 4} />
      <View
        style={{
          flexDirection: "column",
          flexGrow: 1,
          height: "100%",
          padding: 16,
          gap: 10,
        }}
      >
        <Text fontSize={20} color={FG}>session: craie-bench</Text>
        <Message
          who="you"
          text={`Render this list natively. Tick ${tick}.`}
        />
        <Message
          who="agent"
          accent
          text="Done — one flat transaction per commit, decoded off-thread, applied to a retained host."
        />
        <View style={{ flexGrow: 1 }} />
        <Composer tick={tick} />
      </View>
    </View>
  )
}

const port = Number(process.env.CRAIE_PORT ?? 9470)
const transport = await connect(port)
const root = createRoot(transport)
root.render(<App />)
console.log(`[demo] connected on ${port}; ticking`)
setTimeout(() => process.exit(0), 60_000)
