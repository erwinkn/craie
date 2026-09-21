// Smoke test host: opens the native window, runs smoke-worker.tsx, exits
// when the worker closes the session (expected reason: "smoke done").
//   pnpm --dir examples/js build && node examples/js/dist/smoke.mjs
import { loadBindings, runApp } from "@craie/bridge"

try {
  await runApp(loadBindings(), new URL("./smoke-worker.mjs", import.meta.url), {
    title: "craie smoke",
    width: 480,
    height: 320,
  })
} catch (e) {
  const msg = e instanceof Error ? e.message : String(e)
  if (msg === "smoke done") {
    console.log("[smoke] PASS")
    process.exit(0)
  }
  console.error("[smoke] FAIL:", msg)
  process.exit(1)
}
console.error("[smoke] FAIL: window closed before session ended")
process.exit(1)
