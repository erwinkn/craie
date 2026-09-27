// E19 host: runs the app (app.tsx) until the native probe has drawn
// every answer and closed the session with "e19 done" (see e19.sh).

import { runApp } from "@craie/react"

try {
  await runApp(new URL("./app.mjs", import.meta.url), {
    title: "E19 — event round trip",
    width: 900,
    height: 700,
  })
} catch (error) {
  if (!(error instanceof Error && error.message === "e19 done")) throw error
}
