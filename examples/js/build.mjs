// Bundles the example entries for Node: the app worker runs .mjs so
// `new Worker(url)` loads it as ESM, and parameter properties/JSX in the
// TypeScript sources are compiled away.
import { build } from "esbuild"

await build({
  entryPoints: ["host.ts", "demo.tsx", "smoke.ts", "smoke-worker.tsx"],
  bundle: true,
  platform: "node",
  format: "esm",
  jsx: "automatic",
  outdir: "dist",
  outExtension: { ".js": ".mjs" },
})
