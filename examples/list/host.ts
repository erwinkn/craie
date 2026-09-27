// List host entry: opens the native window on the main thread and runs
// the React app (app.tsx) in a worker.
//
//   pnpm --dir examples/list build && node examples/list/dist/host.mjs

import { runApp } from "@craie/react"

await runApp(new URL("./app.mjs", import.meta.url), {
  title: "craie — list",
  width: 520,
  height: 720,
})
