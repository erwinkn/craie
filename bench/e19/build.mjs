import { build } from "esbuild"

await build({
  entryPoints: ["host.ts", "app.tsx"],
  bundle: true,
  platform: "node",
  format: "esm",
  jsx: "automatic",
  outdir: "dist",
  outExtension: { ".js": ".mjs" },
  // React's production build: the development one checks every element.
  define: { "process.env.NODE_ENV": '"production"' },
})
