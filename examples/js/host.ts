// Host entry for the demo: opens the native window on the main thread
// and runs the React app (demo.tsx) in a worker.
//
//   cargo build -p craie-node && cp target/debug/libcraie_node.dylib craie-node.node
//   bun examples/js/host.ts

import { loadBindings, runApp } from "@craie/bridge"

await runApp(loadBindings(), new URL("./demo.tsx", import.meta.url), {
  title: "craie — react",
  width: 900,
  height: 640,
})
