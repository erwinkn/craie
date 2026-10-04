// The kit's elevation tokens on Craie: opens a window, renders them
// (elevation-worker.tsx), writes the frame at rest to `$CRAIE_SHOT`
// (default elevation.png) and exits.
//   pnpm build:native && pnpm --dir examples/js build && node examples/js/dist/elevation.mjs
import { loadBindings, runApp } from "@craie/bridge"

try {
  await runApp(loadBindings(), new URL("./elevation-worker.mjs", import.meta.url), {
    title: "craie — elevation",
    width: 640,
    height: 360,
  })
} catch (e) {
  const msg = e instanceof Error ? e.message : String(e)
  if (msg !== "done") {
    console.error(msg)
    process.exit(1)
  }
}
