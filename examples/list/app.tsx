// A 100,000-message thread in a virtualized list. React renders only the
// rows native reports; native estimates every other row from its text
// length, measures rendered rows, and keeps the thread anchored to the
// end as messages arrive (ScrollView anchor="stick-to-end").
//
//   pnpm --dir examples/list build && node examples/list/dist/host.mjs

import React, { useEffect, useRef, useState } from "react"
import { attachApp, List, ScrollView, Text, View, type HostNode } from "@craie/react"

const BG = "#141518"
const MINE = "#2b4a6b"
const THEIRS = "#22252e"
const FG = "#ececf0"
const DIM = "#9aa0ae"

interface Message {
  id: number
  mine: boolean
  text: string
}

const WORDS = "the list lays out only the rows it renders and measures them once native estimates the rest from text length".split(" ")

function message(id: number): Message {
  // Deterministic lengths: 2 to 60 words.
  const n = 2 + ((id * 7919) % 59)
  const words: string[] = []
  for (let i = 0; i < n; i++) words.push(WORDS[(id + i * 3) % WORDS.length]!)
  return { id, mine: id % 3 === 0, text: `#${id} ${words.join(" ")}` }
}

// Row metrics, shared by the rows and their native estimates.
const PAD_X = 12, PAD_Y = 8, GAP = 6, SIDE = 16, INDENT = 48, FONT = 14

function Row({ m }: { m: Message }) {
  return (
    <View style={{ padding: { left: SIDE, right: SIDE, top: GAP / 2, bottom: GAP / 2 } }}>
      <View
        backgroundColor={m.mine ? MINE : THEIRS}
        borderRadius={10}
        style={{
          padding: { left: PAD_X, right: PAD_X, top: PAD_Y, bottom: PAD_Y },
          margin: m.mine ? { left: INDENT } : { right: INDENT },
        }}
      >
        <Text fontSize={FONT} color={FG}>{m.text}</Text>
      </View>
    </View>
  )
}

function App() {
  const [messages, setMessages] = useState<Message[]>(() =>
    Array.from({ length: 100_000 }, (_, i) => message(i)),
  )
  // Open at the newest message: native clamps the offset to the end of
  // the estimated extent, then holds the end as rows measure.
  const scroller = useRef<HostNode>(null)
  useEffect(() => {
    scroller.current?.scrollTo(0, 1e9)
  }, [])
  // A new message every two seconds; the thread follows at the end.
  useEffect(() => {
    const t = setInterval(() => {
      setMessages((ms) => [...ms, message(ms.length)])
    }, 2000)
    return () => clearInterval(t)
  }, [])
  return (
    <View backgroundColor={BG} style={{ width: "100%", height: "100%" }}>
      <View style={{ padding: { left: SIDE, right: SIDE, top: 14, bottom: 10 } }}>
        <Text fontSize={20} color={FG}>thread</Text>
        <Text fontSize={12} color={DIM}>
          {`${messages.length.toLocaleString("en-US")} messages · virtualized`}
        </Text>
      </View>
      <ScrollView ref={scroller} anchor="stick-to-end" style={{ flexGrow: 1, flexShrink: 1 }}>
        <List<Message>
          items={messages}
          keyOf={(m) => m.id}
          renderItem={(m) => <Row m={m} />}
          templates={[{
            base: GAP + 2 * PAD_Y,
            inset: 2 * SIDE + 2 * PAD_X + INDENT,
            fontSize: FONT,
          }]}
          describe={(m) => ({ template: 0, textLength: m.text.length })}
          overscan={400}
        />
      </ScrollView>
    </View>
  )
}

const root = attachApp()
root.render(<App />)
console.log("[list] attached")
