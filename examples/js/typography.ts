// Text alignment, tabular digits and line limits: opens a window, renders them
// (typography-worker.tsx), writes the frame at rest to `$CRAIE_SHOT`
// (default typography.png) and exits.
//   pnpm build:native && pnpm --dir examples/js build && node examples/js/dist/typography.mjs
import { loadBindings, runApp } from "@craie/bridge"

try {
  await runApp(loadBindings(), new URL("./typography-worker.mjs", import.meta.url), {
    title: "craie — typography",
    width: 360,
    height: 560,
  })
} catch (e) {
  const msg = e instanceof Error ? e.message : String(e)
  if (msg !== "done") {
    console.error(msg)
    process.exit(1)
  }
}
