import { test, expect } from "bun:test"
import { createElement, useState } from "react"
import { createRoot, TextInput, View, useHotkeys } from "../src/index.js"
import type { Hotkey, KeyClaim, Transport, UiEvent } from "../src/host.js"
import { CHORD_FLAG, CLAIM_KIND, EVENT_KIND, KEY_CODE, MODS, NIL, parseChord } from "../src/wire.js"
import { readFrame } from "./crw2.js"

class FakeTransport implements Transport {
  frames: Uint8Array[] = []
  ackCb: ((seq: number) => void) | null = null
  eventCb: ((ev: UiEvent) => void) | null = null
  send(frame: Uint8Array) { this.frames.push(frame.slice()) }
  onAck(cb: (seq: number) => void) { this.ackCb = cb }
  onEvent(cb: (ev: UiEvent) => void) { this.eventCb = cb }
  close() {}
  ops(i = -1) { return readFrame(this.frames.at(i)!).ops }
  seq(i = -1) { return Number(readFrame(this.frames.at(i)!).seq) }
  all() { return this.frames.flatMap(f => readFrame(f).ops) }
}

const tick = () => new Promise(r => setTimeout(r, 0))
const ctrl = (key: string) => ({ kind: CLAIM_KIND.key, flags: 0, mods: MODS.ctrl, key: key.codePointAt(0)! })
/** `mod+key` as the host parses it on this platform. */
const mod = (key: string) => ({ ...ctrl(key), mods: process.platform === "darwin" ? MODS.meta : MODS.ctrl })

function claim(node: number, version: number, kind: number, index: number, text = "", x = 0, y = 0): UiEvent {
  return { kind: EVENT_KIND.claim, node, generation: 0, revision: version, x, y, a: 0, b: 0, key: kind | index << 8, text }
}

test("chords parse as the web's key names with exact modifiers", () => {
  expect(parseChord("mod+s", true)).toEqual({ kind: CLAIM_KIND.key, flags: 0, mods: MODS.meta, key: 0x73 })
  expect(parseChord("mod+s", false)).toEqual(ctrl("s"))
  expect(parseChord("Mod+Shift+O", false)).toEqual({ ...ctrl("o"), mods: MODS.ctrl | MODS.shift })
  expect(parseChord("shift+?", false)).toEqual({ kind: CLAIM_KIND.key, flags: 0, mods: MODS.shift, key: 0x3f })
  expect(parseChord("escape", false)).toEqual({ kind: CLAIM_KIND.key, flags: CHORD_FLAG.named, mods: 0, key: KEY_CODE.escape! })
  expect(parseChord("alt+arrowup", false)!.mods).toBe(MODS.alt)
  expect(parseChord("f12", false)!.key).toBe(43)
  expect(parseChord("space", false)).toEqual(parseChord(" ", false))
  expect(parseChord("mod++", false)).toEqual(ctrl("+"))
  expect(parseChord("+", false)!.key).toBe(0x2b)
  expect(parseChord("ctrl+meta+k", false)!.mods).toBe(MODS.ctrl | MODS.meta)
  // Non-BMP characters are one key.
  expect(parseChord("😀", false)!.key).toBe(0x1f600)
  for (const apple of [false, true]) {
    for (const bad of ["hyper+k", "mod+", "ab", "", "toString+k", "mod+constructor", "__proto__"]) {
      expect(parseChord(bad, apple)).toBeNull()
    }
  }
})

test("a keymap is a versioned claim set; the handlers follow the latest render", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const runs: string[] = []
  const App = ({ tag, keys }: { tag: string; keys: KeyClaim[] }) =>
    createElement(View, { keymap: keys.map(k => ({ ...k, run: () => runs.push(`${tag}:${k.keys}`) })) })

  root.renderSync(createElement(App, { tag: "a", keys: [{ keys: "mod+k", run() {} }, { keys: "escape", run() {}, repeat: false }] }))
  await tick()
  const op = t.ops().find(o => o.tag === 0x61)!
  const id = op.id
  expect(op.f).toEqual([1])
  expect(op.claims).toEqual([
    mod("k"),
    { kind: CLAIM_KIND.key, flags: CHORD_FLAG.named | CHORD_FLAG.noRepeat, mods: 0, key: KEY_CODE.escape! },
  ])

  // Same chords, new closures: no op, and version 1 runs the new ones.
  root.renderSync(createElement(App, { tag: "b", keys: [{ keys: "mod+k", run() {} }, { keys: "escape", run() {}, repeat: false }] }))
  await tick()
  expect(t.all().filter(o => o.tag === 0x61)).toHaveLength(1)
  t.eventCb!(claim(id, 1, CLAIM_KIND.key, 1))
  expect(runs).toEqual(["b:escape"])

  // New chords: version 2. Until native acks it, version 1 still runs
  // (an event raised before the apply names it).
  root.renderSync(createElement(App, { tag: "c", keys: [{ keys: "mod+j", run() {} }] }))
  await tick()
  const v2 = t.ops().find(o => o.tag === 0x61)!
  expect(v2.f).toEqual([2])
  expect(v2.claims).toEqual([mod("j")])
  t.eventCb!(claim(id, 1, CLAIM_KIND.key, 0))
  t.eventCb!(claim(id, 2, CLAIM_KIND.key, 0))
  expect(runs).toEqual(["b:escape", "b:mod+k", "c:mod+j"])
  t.ackCb!(t.seq())
  t.eventCb!(claim(id, 1, CLAIM_KIND.key, 0))
  t.eventCb!(claim(id, 2, CLAIM_KIND.key, 0))
  expect(runs).toEqual(["b:escape", "b:mod+k", "c:mod+j", "c:mod+j"])

  // `when: false` leaves the claim out; an empty set is sent (native
  // drops it) with a new version.
  root.renderSync(createElement(App, { tag: "d", keys: [{ keys: "mod+j", run() {}, when: false }] }))
  await tick()
  const v3 = t.ops().find(o => o.tag === 0x61)!
  expect(v3.f).toEqual([3])
  expect(v3.claims).toEqual([])
})

test("an unknown chord or submit key is reported once and skipped", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const errors = console.error
  const logged: string[] = []
  console.error = (m: string) => logged.push(m)
  const App = ({ n }: { n: number }) => createElement(View, null,
    createElement(View, { keymap: [{ keys: "hyper+k", run() {} }, { keys: "k", run() {} }] }),
    createElement(TextInput, { onSubmit() {}, submitKey: "shift+enter" as any, placeholder: `${n}` }))
  try {
    root.renderSync(createElement(App, { n: 1 }))
    await tick()
    root.renderSync(createElement(App, { n: 2 }))
    await tick()
  } finally {
    console.error = errors
  }
  expect(logged).toEqual(['craie: unknown key chord "hyper+k"', 'craie: unknown submitKey "shift+enter", using "enter"'])
  const ops = t.frames.flatMap(f => readFrame(f).ops)
  expect(ops.find(o => o.tag === 0x61)!.claims).toEqual([{ kind: CLAIM_KIND.key, flags: 0, mods: 0, key: 0x6b }])
  expect(ops.find(o => o.tag === 0x41)!.f[1]).toBe(0) // single line, enter
})

test("clipboard claims answer with commands", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let paste: string | undefined = "PASTED"
  root.renderSync(createElement(TextInput, {
    onPaste: e => { expect(e.text).toBe("clip"); return paste },
    onCopy: e => e.text.toUpperCase(),
    onCut: e => `cut ${e.text}`,
  }))
  await tick()
  const op = t.ops().find(o => o.tag === 0x61)!
  const id = op.id
  expect(op.claims!.map(c => c.kind)).toEqual([CLAIM_KIND.paste, CLAIM_KIND.copy, CLAIM_KIND.cut])
  const commands = () => t.ops().filter(o => o.tag === 0x80).map(o => [o.id, o.f[0], o.s])

  t.eventCb!(claim(id, 1, CLAIM_KIND.paste, 0, "clip"))
  await tick()
  expect(commands()).toEqual([[id, 4, "PASTED"]])
  // No answer: nothing is inserted.
  paste = undefined
  const before = t.frames.length
  t.eventCb!(claim(id, 1, CLAIM_KIND.paste, 0, "clip"))
  await tick()
  expect(t.frames.length).toBe(before)

  t.eventCb!(claim(id, 1, CLAIM_KIND.copy, 1, "sel"))
  await tick()
  expect(commands()).toEqual([[NIL, 5, "SEL"]])
  // Cut writes the answer and deletes the selection.
  t.eventCb!(claim(id, 1, CLAIM_KIND.cut, 2, "sel"))
  await tick()
  expect(commands()).toEqual([[NIL, 5, "cut sel"], [id, 4, ""]])
})

test("drop and context-menu claims carry their payloads", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const got: unknown[] = []
  root.renderSync(createElement(View, {
    onDrop: e => got.push(e.paths, e.x, e.y),
    onContextMenu: e => got.push("menu", e.x, e.y),
  }))
  await tick()
  const id = t.ops().find(o => o.tag === 0x61)!.id
  t.eventCb!(claim(id, 1, CLAIM_KIND.drop, 0, "/a b.png\0/c\n.txt", 3, 4))
  t.eventCb!(claim(id, 1, CLAIM_KIND.contextMenu, 1, "", 5, 6))
  expect(got).toEqual([["/a b.png", "/c\n.txt"], 3, 4, "menu", 5, 6])
})

test("claim events for a previous occupant of the id are dropped", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const runs: string[] = []
  const App = ({ k }: { k: string }) =>
    createElement(View, null, createElement(View, { key: k, keymap: [{ keys: "x", run: () => runs.push(k) }] }))
  root.renderSync(createElement(App, { k: "a" }))
  await tick()
  const id = t.ops().find(o => o.tag === 0x61)!.id
  root.renderSync(createElement(App, { k: "b" }))
  await tick()
  root.renderSync(createElement(App, { k: "c" }))
  await tick()
  // "c" reuses "a"'s id at generation 1, with its own version 1.
  expect(t.ops().find(o => o.tag === 0x61)!.id).toBe(id)
  t.eventCb!(claim(id, 1, CLAIM_KIND.key, 0))
  t.eventCb!({ ...claim(id, 1, CLAIM_KIND.key, 0), generation: 1 })
  expect(runs).toEqual(["c"])
})

test("hotkeys form the window list, the latest mounted hook first", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const runs: string[] = []
  const Keys = ({ tag, list }: { tag: string; list: Hotkey[] }) => {
    useHotkeys(list.map(h => ({ ...h, run: () => runs.push(`${tag}:${h.keys}`) })))
    return null
  }
  const App = ({ second }: { second: boolean }) =>
    createElement(View, null,
      createElement(Keys, { tag: "a", list: [{ keys: "/", run() {} }] }),
      second ? createElement(Keys, { tag: "b", list: [{ keys: "mod+k", run() {}, allowInInput: true }, { keys: "/", run() {} }] }) : null)

  root.renderSync(createElement(App, { second: true }))
  await tick()
  let op = t.all().filter(o => o.tag === 0x61).at(-1)!
  expect(op.id).toBe(NIL)
  expect(op.f).toEqual([1])
  // b's "/" comes before a's: native runs the first match.
  expect(op.claims).toEqual([
    { ...mod("k"), flags: CHORD_FLAG.inInput },
    { kind: CLAIM_KIND.key, flags: 0, mods: 0, key: 0x2f },
    { kind: CLAIM_KIND.key, flags: 0, mods: 0, key: 0x2f },
  ])
  t.eventCb!(claim(NIL, 1, CLAIM_KIND.key, 0))
  t.eventCb!(claim(NIL, 1, CLAIM_KIND.key, 1))
  expect(runs).toEqual(["b:mod+k", "b:/"])

  // Unmounting one hook resends the list; version 1 runs until native
  // acks the transaction that replaced it. An unchanged list sends
  // nothing.
  root.renderSync(createElement(App, { second: false }))
  await tick()
  op = t.all().filter(o => o.tag === 0x61).at(-1)!
  expect(op.f).toEqual([2])
  expect(op.claims).toHaveLength(1)
  t.eventCb!(claim(NIL, 1, CLAIM_KIND.key, 1))
  t.ackCb!(t.seq())
  t.eventCb!(claim(NIL, 1, CLAIM_KIND.key, 1))
  t.eventCb!(claim(NIL, 2, CLAIM_KIND.key, 0))
  expect(runs).toEqual(["b:mod+k", "b:/", "b:/", "a:/"])
  const frames = t.frames.length
  root.renderSync(createElement(App, { second: false }))
  await tick()
  expect(t.frames.slice(frames).flatMap(f => readFrame(f).ops).some(o => o.tag === 0x61)).toBe(false)
})

test("Enter submits only with onSubmit, per submitKey", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const flags = () => t.ops().find(o => o.tag === 0x41)?.f[1]
  const App = (props: Record<string, unknown>) => createElement(TextInput, { multiline: true, ...props })
  root.renderSync(createElement(App, {}))
  await tick()
  expect(flags()).toBe(1 | 2 << 1) // multiline, none
  root.renderSync(createElement(App, { onSubmit() {} }))
  await tick()
  expect(flags()).toBe(1) // multiline, enter
  root.renderSync(createElement(App, { onSubmit() {}, submitKey: "none" }))
  await tick()
  expect(flags()).toBe(1 | 2 << 1)
  root.renderSync(createElement(App, { onSubmit() {}, submitKey: "mod+enter" }))
  await tick()
  expect(flags()).toBe(1 | 1 << 1)
  // A new closure changes nothing.
  root.renderSync(createElement(App, { onSubmit() {}, submitKey: "mod+enter" }))
  await tick()
  expect(flags()).toBeUndefined()
})

test("key records decode modifiers, repeat, composition and the physical key", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  const got: unknown[] = []
  root.renderSync(createElement(View, { focusable: true, onKeyDown: e => got.push(e) }))
  await tick()
  const id = t.ops().find(o => o.tag === 0x01)!.id
  const down = (key: number, text = "") =>
    t.eventCb!({ kind: EVENT_KIND.keyDown, node: id, generation: 0, revision: 0, x: 0, y: 0, a: 0, b: 0, key, text })
  down(MODS.shift | MODS.alt | 1 << 4 | 0x63 << 16, "Ç")
  down(1 << 5 | 0x63 << 16, "с")
  down(KEY_CODE.arrowup! << 8)
  down(KEY_CODE.f13! << 8)
  down(0x37 << 16, "7")
  down(0x2f << 16, "/")
  down(0)
  expect(got).toMatchObject([
    { key: 0, char: "Ç", shift: true, ctrl: false, alt: true, meta: false, repeat: true, composing: false, code: "KeyC" },
    { key: 0, char: "с", composing: true, code: "KeyC" },
    { key: KEY_CODE.arrowup, code: "ArrowUp" },
    { key: KEY_CODE.f13, code: "F13" },
    { code: "Digit7" },
    { code: "Slash" },
    { code: "" },
  ])
})

test("a claim's update renders before the next event in the batch", async () => {
  const t = new FakeTransport()
  const root = createRoot(t)
  let seen: number[] = []
  const App = () => {
    const [n, setN] = useState(0)
    return createElement(View, { keymap: [{ keys: "x", run: () => { seen.push(n); setN(n + 1) } }] })
  }
  root.renderSync(createElement(App))
  await tick()
  const id = t.ops().find(o => o.tag === 0x61)!.id
  t.eventCb!(claim(id, 1, CLAIM_KIND.key, 0))
  t.eventCb!(claim(id, 1, CLAIM_KIND.key, 0))
  expect(seen).toEqual([0, 1])
})

test("without acks, a replaced version's handlers go at once", async () => {
  const t = new FakeTransport()
  const root = createRoot({ send: f => t.send(f), onEvent: cb => t.onEvent(cb), close() {} })
  const runs: string[] = []
  const App = ({ k }: { k: string }) => createElement(View, { keymap: [{ keys: k, run: () => runs.push(k) }] })
  root.renderSync(createElement(App, { k: "x" }))
  await tick()
  const id = t.ops().find(o => o.tag === 0x61)!.id
  root.renderSync(createElement(App, { k: "y" }))
  await tick()
  t.eventCb!(claim(id, 1, CLAIM_KIND.key, 0))
  t.eventCb!(claim(id, 2, CLAIM_KIND.key, 0))
  expect(runs).toEqual(["y"])
})
