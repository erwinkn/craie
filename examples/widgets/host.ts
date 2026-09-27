// Widgets host: opens the window on the main thread and runs the React
// app in a worker. The sparkline is a native Bars surface; no painter is
// registered here.
//
//   pnpm build:native && pnpm --dir examples/widgets build
//   node examples/widgets/dist/host.mjs

import { runApp } from "@craie/react"

await runApp(new URL("./app.mjs", import.meta.url), {
  title: "craie — widgets",
  width: 560,
  height: 480,
})
