// Todo app: runs in the application worker. State lives in React;
// persistence is an async JSON file write debounced after each change.
// The native host owns layout, text, pointer dispatch, focus, scrolling,
// and text editing — this file never touches a DOM-like API.
//
//   pnpm --dir examples/todo build && node examples/todo/dist/host.mjs

import React, { useEffect, useRef, useState } from "react"
import { attachApp, ScrollView, Text, TextInput, View, type HostNode } from "@craie/react"
import { mkdir, readFile, writeFile } from "node:fs/promises"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const STORE = join(dirname(fileURLToPath(import.meta.url)), "todos.json")

const BG = "#141518"
const PANEL = "#1b1d24"
const EDGE = "#2a2d38"
const FG = "#ececf0"
const DIM = "#9aa0ae"
const ACCENT = "#6dc7ff"
const DONE = "#5a5f6e"

interface Todo {
  id: number
  text: string
  done: boolean
}

async function load(): Promise<Todo[]> {
  try {
    const raw = await readFile(STORE, "utf8")
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : []
  } catch {
    return []
  }
}

let saveTimer: ReturnType<typeof setTimeout> | undefined
function save(todos: Todo[]) {
  clearTimeout(saveTimer)
  saveTimer = setTimeout(async () => {
    try {
      await mkdir(dirname(STORE), { recursive: true })
      await writeFile(STORE, JSON.stringify(todos, null, 2))
    } catch (e) {
      console.error("[todo] save failed:", e)
    }
  }, 300)
}

function Row({
  todo,
  onToggle,
  onDelete,
}: {
  todo: Todo
  onToggle: (id: number) => void
  onDelete: (id: number) => void
}) {
  return (
    <View
      backgroundColor={PANEL}
      borderRadius={8}
      style={{
        flexDirection: "row",
        alignItems: "center",
        padding: { left: 12, right: 8, top: 9, bottom: 9 },
        gap: 10,
        width: "100%",
      }}
    >
      <View
        backgroundColor={todo.done ? ACCENT : "#00000000"}
        borderColor={todo.done ? ACCENT : DIM}
        borderWidth={1.5}
        borderRadius={9}
        onPointerDown={() => onToggle(todo.id)}
        style={{ width: 18, height: 18 }}
      />
      <View
        onPointerDown={() => onToggle(todo.id)}
        style={{ flexGrow: 1 }}
      >
        <Text fontSize={14} color={todo.done ? DONE : FG}>
          {todo.text}
        </Text>
      </View>
      <View
        borderRadius={6}
        onPointerDown={() => onDelete(todo.id)}
        style={{ padding: { left: 8, right: 8, top: 2, bottom: 2 } }}
      >
        <Text fontSize={13} color={DIM}>✕</Text>
      </View>
    </View>
  )
}

function App({ initial }: { initial: Todo[] }) {
  const [todos, setTodos] = useState(initial)
  const [draft, setDraft] = useState("")
  const nextId = useRef(initial.reduce((m, t) => Math.max(m, t.id), 0) + 1)
  const input = useRef<HostNode | null>(null)

  useEffect(() => save(todos), [todos])

  const add = (text: string) => {
    const trimmed = text.trim()
    if (!trimmed) return
    setTodos((ts) => [...ts, { id: nextId.current++, text: trimmed, done: false }])
    setDraft("")
  }
  const toggle = (id: number) =>
    setTodos((ts) => ts.map((t) => (t.id === id ? { ...t, done: !t.done } : t)))
  const remove = (id: number) => setTodos((ts) => ts.filter((t) => t.id !== id))
  const clearDone = () => setTodos((ts) => ts.filter((t) => !t.done))

  const open = todos.filter((t) => !t.done).length

  return (
    <View
      backgroundColor={BG}
      style={{ width: "100%", height: "100%", padding: 20, gap: 12 }}
    >
      <Text fontSize={24} color={FG}>todo</Text>
      <TextInput
        ref={input}
        value={draft}
        onChangeText={setDraft}
        onSubmit={add}
        placeholder="What needs doing?"
        fontSize={14}
        color={FG}
        backgroundColor={PANEL}
        borderColor={EDGE}
        borderWidth={1}
        borderRadius={8}
        style={{ padding: { left: 12, right: 12, top: 9, bottom: 9 }, width: "100%" }}
      />
      <ScrollView style={{ flexGrow: 1, width: "100%" }}>
        <View style={{ gap: 6, width: "100%" }}>
          {todos.map((t) => (
            <Row key={t.id} todo={t} onToggle={toggle} onDelete={remove} />
          ))}
          {todos.length === 0 && (
            <Text fontSize={14} color={DIM}>Nothing yet — type above and press enter.</Text>
          )}
        </View>
      </ScrollView>
      <View
        style={{
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          width: "100%",
        }}
      >
        <Text fontSize={12} color={DIM}>
          {open} open · {todos.length} total
        </Text>
        <View onPointerDown={clearDone} style={{ padding: 4 }}>
          <Text fontSize={12} color={DIM}>clear completed</Text>
        </View>
      </View>
    </View>
  )
}

const root = attachApp()
const initial = await load()
root.render(<App initial={initial} />)
console.log(`[todo] attached; ${initial.length} items loaded`)
