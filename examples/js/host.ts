// Host entry for the demo: opens the native window on the main thread
// and runs the React app (demo) in a worker. Runs under Node — Bun
// cannot require N-API addons inside worker threads.
//
//   cargo build -p craie-node && cp target/debug/libcraie_node.dylib craie-node.node
//   pnpm --dir examples/js build && node examples/js/dist/host.mjs

import { loadBindings, runApp } from "@craie/bridge"

await runApp(loadBindings(), new URL("./demo.mjs", import.meta.url), {
  title: "craie — react",
  width: 900,
  height: 640,
})
