// Text alignment, tabular digits and line limits, on a page; the digits
// in Inter, registered from the repository's subset.
import { createElement as h } from "react"
import { readFileSync } from "node:fs"
import { workerData, parentPort } from "node:worker_threads"
import { createRoot, loadBindings, NativeTransport, Text, View } from "@craie/bridge"

const client = new (loadBindings().NativeClient)(workerData.craieSession)
const root = createRoot(new NativeTransport(client))

const long = "The quick brown fox jumps over the lazy dog, then keeps on running through the field"
const box = { width: 260, padding: 8, borderRadius: 6 }
const label = (s: string) => h(Text, { fontSize: 11, color: "#71717a" }, s)

function App() {
  return h(View, { backgroundColor: "#fafafa", style: { width: "100%", height: "100%", padding: 24, gap: 14 } },
    label("textAlign: left, center, right"),
    ...(["left", "center", "right"] as const).map(a =>
      h(View, { key: a, backgroundColor: "#ffffff", style: box },
        h(Text, { textAlign: a, fontSize: 14, color: "#18181b" }, `Aligned ${a}`))),
    label("Inter, registered: proportional, then tabular-nums"),
    h(View, { backgroundColor: "#ffffff", style: { ...box, flexDirection: "row", gap: 24 } },
      h(Text, { fontSize: 14, color: "#18181b", fontFamily: "Inter" }, "1111\n8888"),
      h(Text, { fontSize: 14, color: "#18181b", fontFamily: "Inter", fontVariant: ["tabular-nums"] }, "1111\n8888")),
    label("numberOfLines: 1, then 2"),
    h(View, { backgroundColor: "#ffffff", style: box },
      h(Text, { numberOfLines: 1, fontSize: 14, color: "#18181b" }, long)),
    h(View, { backgroundColor: "#ffffff", style: box },
      h(Text, { numberOfLines: 2, fontSize: 14, color: "#18181b" }, long)))
}

// The host runs its loop once ready: signal before awaiting an ack.
parentPort?.postMessage({ craieReady: true })
await root.host.registerFont(readFileSync(new URL("../../../assets/fonts/Inter-Subset-Regular.ttf", import.meta.url)))
root.renderSync(h(App))
const shot = process.env.CRAIE_SHOT ?? "typography.png"
const frame = await root.host.capture(shot, { rest: true })
console.log(`[typography] wrote ${shot} (${frame.width}x${frame.height}, frame ${frame.frame})`)
client.close("done")
