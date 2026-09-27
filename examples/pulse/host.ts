// Pulse host entry: opens the native window on the main thread and runs
// the React app (app.tsx) in a worker.
//
//   pnpm dev:pulse

import { runApp } from "@craie/react"

await runApp(new URL("./app.mjs", import.meta.url), {
  title: "Pulse — React on Craie",
  width: 1440,
  height: 900,
})
