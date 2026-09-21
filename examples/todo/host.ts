// Todo host entry: opens the native window on the main thread and runs
// the React app (app.tsx) in a worker. Runs under Node — Bun cannot
// require N-API addons inside worker threads.
//
//   cargo build -p craie-node && cp target/debug/libcraie_node.dylib craie-node.node
//   pnpm --dir examples/todo build && node examples/todo/dist/host.mjs

import { runApp } from "@craie/react"

await runApp(new URL("./app.mjs", import.meta.url), {
  title: "craie — todo",
  width: 560,
  height: 720,
})
