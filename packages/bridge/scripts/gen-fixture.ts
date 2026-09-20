// Generates test/fixture.bin: one transaction covering every op the Rust
// decoder must accept. Regenerate after wire format changes:
//   bun packages/bridge/scripts/gen-fixture.ts
import { Encoder, NIL } from "../src/wire.js"

const enc = new Encoder()
enc.create(0, 0)                                    // view
enc.create(1, 1)                                    // text
enc.setText(1, "héllo — مرحبا 日本語")
enc.textProps(1, 18.5, 0x6dc7_ff80)
const sid = enc.styleIdFor({
  display: "flex",
  flexDirection: "column",
  gap: 12,
  padding: { left: 16, top: "10%" },
  width: "50%",
  height: "auto",
  minWidth: 100,
  alignItems: "center",
  justifyContent: "space-between",
  flexGrow: 1.5,
  flexShrink: 0.5,
  aspectRatio: 1.25,
  overflow: "hidden",
  position: "absolute",
  inset: { left: 4 },
  margin: { top: "auto" as const },
})
enc.setStyle(0, sid)
enc.viewPaint(0, 0x1b1d_24ff)
enc.place(NIL, 0, NIL)
enc.place(0, 1, NIL)
enc.hidden(1, true)
enc.hidden(1, false)
enc.detach(1)
enc.place(0, 1, NIL)
enc.remove(1)

await Bun.write(new URL("../test/fixture.bin", import.meta.url).pathname, enc.finish(99n))
console.log("wrote fixture.bin")
