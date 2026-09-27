import { execFileSync } from "node:child_process"
import { mkdirSync, readdirSync } from "node:fs"
import { build } from "esbuild"

// SVG icons become Craie vector assets at build time (craie-svg).
mkdirSync("dist/icons", { recursive: true })
for (const file of readdirSync("icons")) {
  if (!file.endsWith(".svg")) continue
  const out = `dist/icons/${file.replace(/\.svg$/, ".crv")}`
  execFileSync(
    "cargo",
    ["run", "--quiet", "--release", "-p", "craie-svg-import", "--bin", "craie-svg", "--", `icons/${file}`, out],
    { stdio: "inherit" },
  )
}

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
