// Widgets host: opens the window on the main thread, registers the
// sparkline painter for <Custom tag={1}>, and runs the React app in a
// worker.
//
//   pnpm build:native && pnpm --dir examples/widgets build
//   node examples/widgets/dist/host.mjs

import { runApp } from "@craie/react"

const BAR = 0x6dc7c8ff
const BAR_MAX = 0x6dc7ffff

await runApp(new URL("./app.mjs", import.meta.url), {
  title: "craie — widgets",
  width: 560,
  height: 480,
  painters: {
    // Sparkline: `text` carries a comma-separated 0..1 series, `data[0]`
    // overrides the bar color, `data[1]` flags the max bar. Emits one
    // quad per sample — logical points, window-absolute.
    1: (spec) => {
      const values = spec.text
        .split(",")
        .map(Number)
        .filter((v) => Number.isFinite(v))
      if (!values.length || spec.w <= 0 || spec.h <= 0) return []
      const color = (spec.data[0] >>> 0) || BAR
      const markMax = spec.data[1] !== 0
      const max = Math.max(...values)
      const bw = spec.w / values.length
      return values.map((v, i) => ({
        x: spec.x + i * bw + 1,
        y: spec.y + spec.h * (1 - Math.min(v, 1)),
        w: Math.max(bw - 2, 1),
        h: spec.h * Math.min(v, 1),
        color: markMax && v === max ? BAR_MAX : color,
        radius: 1.5,
      }))
    },
  },
})
