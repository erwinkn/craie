import { build } from "esbuild"

await build({
  entryPoints: ["host.ts", "app.tsx"],
  bundle: true,
  platform: "node",
  format: "esm",
  jsx: "automatic",
  outdir: "dist",
  outExtension: { ".js": ".mjs" },
})
