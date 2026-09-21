// Smoke test worker: attaches, renders a frame, awaits the native ack,
// exercises an event listener path, then closes the session so the main
// thread's event loop exits.
import React, { createElement } from "react"
import { workerData, parentPort, isMainThread } from "node:worker_threads"
import { createRoot, NativeTransport, loadBindings, View, Text } from "@craie/bridge"

if (isMainThread) throw Error("worker only")
const bindings = loadBindings()
const client = new bindings.NativeClient(workerData.craieSession)
const transport = new NativeTransport(client)
const root = createRoot(transport)

let scrolls = 0
root.render(
  createElement(
    View,
    {
      backgroundColor: "#141518",
      style: { width: "100%", height: "100%", padding: 20 },
      onScroll: () => scrolls++,
    },
    createElement(Text, { fontSize: 16, color: "#ececf0" }, "smoke"),
  ),
)

// The host starts its event loop only after craieReady — signal first,
// then prove the ack frame arrives over subscribe.
parentPort?.postMessage({ craieReady: true })
await root.flush()
console.log("[smoke] first transaction acked")

setTimeout(() => {
  console.log("[smoke] closing session")
  client.close("smoke done")
}, 3000)
