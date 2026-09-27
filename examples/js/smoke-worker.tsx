// Smoke test worker: attaches, renders a frame with claims (a keymap, a
// paste claim, a window hotkey) and an image (fetched, decoded natively,
// reported loaded), awaits the native ack, then closes the session so
// the main thread's event loop exits.
import React, { createElement } from "react"
import { workerData, parentPort, isMainThread } from "node:worker_threads"
import {
  createRoot,
  NativeTransport,
  loadBindings,
  useHotkeys,
  View,
  Text,
  Image,
} from "@craie/bridge"

if (isMainThread) throw Error("worker only")
const bindings = loadBindings()
const client = new bindings.NativeClient(workerData.craieSession)
const transport = new NativeTransport(client)
const root = createRoot(transport)

let scrolls = 0
// 2 x 2: red and blue columns.
const PNG =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4z8AAQv8ZYAwAQ84H+VjtZqAAAAAASUVORK5CYII="
let loaded = ""
function Smoke() {
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

setTimeout(() => {
  console.log("[smoke] closing session")
  client.close(loaded === "2x2" ? "smoke done" : "image did not load")
}, 3000)
