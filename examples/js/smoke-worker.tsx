// Smoke test worker: attaches, renders a frame with claims (a keymap, a
// paste claim, a window hotkey), state styles (scopes, variants, an
// inherited color, a portal) and an image (fetched, decoded natively,
// reported loaded), awaits the native ack, changes a state and awaits
// that ack, then closes the session so the main thread's event loop
// exits.
import React, { createElement } from "react"
import { workerData, parentPort, isMainThread } from "node:worker_threads"
import {
  createRoot,
  defineStates,
  NativeTransport,
  loadBindings,
  useHotkeys,
  Portal,
  Pressable,
  View,
  Text,
  Image,
} from "@craie/bridge"

if (isMainThread) throw Error("worker only")
const bindings = loadBindings()
const client = new bindings.NativeClient(workerData.craieSession)
const transport = new NativeTransport(client)
const root = createRoot(transport)

defineStates(["unread", "streaming"])

let scrolls = 0
// 2 x 2: red and blue columns.
const PNG =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4z8AAQv8ZYAwAQ84H+VjtZqAAAAAASUVORK5CYII="
let loaded = ""
function Row({ selected, unread }: { selected: boolean; unread: boolean }) {
  return createElement(
    Pressable,
    {
      group: "row",
      selected,
      states: { unread },
      backgroundColor: "#1b1d22",
      style: {
        height: 36,
        padding: { left: 12, right: 12 },
        flexDirection: "row",
        alignItems: "center",
        transition: { backgroundColor: { duration: 0.12 }, color: { duration: 0.12 } },
      },
      _hover: { backgroundColor: "#24272e" },
      _selected: { backgroundColor: "#2d3240", _hover: { backgroundColor: "#343a4a" } },
      _narrow: { style: { height: 44 } },
    },
    createElement(Text, { color: "#9aa0aa", _unread: { color: "#ffffff" } }, "a thread"),
    createElement(View, {
      style: { width: 8, height: 8, opacity: 0 },
      backgroundColor: "#6dc7ff",
      _row: { _hover: { style: { opacity: 1 } } },
      _touch: { style: { opacity: 1 } },
    }),
    createElement(Portal, {}, createElement(View, {
      style: { position: "absolute", width: 4, height: 4, opacity: 0 },
      _row: { _selected: { style: { opacity: 1 } } },
    })),
  )
}

let setUnread: (v: boolean) => void = () => {}
function Smoke() {
  const [unread, set] = React.useState(true)
  setUnread = set
  useHotkeys([{ keys: "mod+k", run: () => {}, allowInInput: true }])
  return createElement(
    View,
    {
      backgroundColor: "#141518",
      style: { width: "100%", height: "100%", padding: 20 },
      onScroll: () => scrolls++,
      keymap: [{ keys: "escape", run: () => {}, repeat: false }],
      onPaste: () => undefined,
    },
    createElement(Text, { fontSize: 16, color: "#ececf0" }, "smoke"),
    createElement(Row, { selected: true, unread }),
    createElement(Image, {
      src: PNG,
      fit: "contain",
      style: { width: 40, height: 40 },
      onLoad: (e) => {
        loaded = `${e.width}x${e.height}`
        console.log(`[smoke] image loaded: ${loaded}`)
      },
      onError: (e) => console.error(`[smoke] image failed: ${e.message}`),
    }),
  )
}
root.render(createElement(Smoke))

// The host starts its event loop only after craieReady — signal first,
// then prove the ack frame arrives over subscribe.
parentPort?.postMessage({ craieReady: true })
await root.flush()
console.log("[smoke] first transaction acked")
setUnread(false)
await new Promise(r => setTimeout(r, 0))
await root.flush()
console.log("[smoke] state change acked")

setTimeout(() => {
  console.log("[smoke] closing session")
  client.close(loaded === "2x2" ? "smoke done" : "image did not load")
}, 3000)
