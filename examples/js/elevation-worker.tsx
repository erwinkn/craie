// The kit's elevation tokens (packages/ui/src/theme/tokens.ts in
// Marbre, light theme) as structured box shadows, on a page.
import { createElement as h } from "react"
import { workerData, parentPort } from "node:worker_threads"
import { createRoot, loadBindings, NativeTransport, Text, View, type BoxShadow } from "@craie/bridge"

const client = new (loadBindings().NativeClient)(workerData.craieSession)
const root = createRoot(new NativeTransport(client))

const line = "#e4e4e7", strong = "#d4d4d8", accent = "#3b82f6"
const shadow = (a: number) => Math.round(a * 255) // alpha of the shadow color (black)
const drop = (y: number, blur: number, a: number, spread = 0): BoxShadow =>
  ({ offsetY: y, blurRadius: blur, spreadDistance: spread, color: shadow(a) })
const ring = (color: string, width = 1): BoxShadow => ({ spreadDistance: width, color })

const sm = [drop(18, 47, 0.03), drop(7.5, 19, 0.02), drop(4, 10.5, 0.02), drop(2.3, 5.8, 0.01), drop(1.2, 3.1, 0.01), drop(0.5, 1.3, 0.01)]
const md = [drop(17.54, 23.39, 0.04), drop(9.4, 12.5, 0.03), drop(5.25, 7, 0.02), drop(2.79, 3.72, 0.01, -2), drop(1.16, 1.5, 0.01)]
const lg = [drop(25, 50, 0.05), drop(12, 24, 0.04), drop(6, 12, 0.03), drop(3, 6, 0.02), drop(1.5, 3, 0.02)]

const tokens: [string, BoxShadow[]][] = [
  ["hairline", [ring(line)]],
  ["button", [ring(strong), drop(0, 4, 0.04)]],
  ["card", [ring(line), ...sm]],
  ["raised", [ring(line), ...md]],
  ["overlay", [ring(line), ...lg]],
  ["ring-accent", [ring(accent)]],
  ["inset-field", [{ offsetY: 1, blurRadius: 2, color: shadow(0.12), inset: true }]],
  ["filled", [{ offsetY: 1, color: 0xffffff24, inset: true }]],
]

function App() {
  return h(View, {
    backgroundColor: "#fafafa",
    style: { width: "100%", height: "100%", padding: 32, flexDirection: "row", flexWrap: "wrap", gap: 32 },
  }, tokens.map(([name, boxShadow]) =>
    h(View, {
      key: name,
      backgroundColor: name === "filled" ? "#18181b" : "#ffffff",
      borderRadius: 10,
      boxShadow,
      style: { width: 120, height: 120, alignItems: "center", justifyContent: "center" },
    }, h(Text, { fontSize: 13, color: name === "filled" ? "#fafafa" : "#3f3f46" }, name))))
}

root.renderSync(h(App))
parentPort?.postMessage({ craieReady: true })
const shot = process.env.CRAIE_SHOT ?? "elevation.png"
const frame = await root.host.capture(shot, { rest: true })
console.log(`[elevation] wrote ${shot} (${frame.width}x${frame.height}, frame ${frame.frame})`)
client.close("done")
