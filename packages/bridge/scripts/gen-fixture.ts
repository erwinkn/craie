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
enc.paint(0, 0x1122_33ff, 6.5, { color: 0xff00_00ff, width: 2 }) // masked paint
enc.create(2, 2)                                    // input
enc.inputProps(2, 15, 0xffff_ffff, "type here", true)
enc.props(2, 0x1ff, true)                           // all listeners, focusable
enc.create(3, 3)                                    // custom
enc.custom(3, 7, [0.25, 0.5, 0.75, 1.0], "0.1,0.4,0.9")
enc.paint(3, 0x1b1d_24ff, 4, undefined)
enc.label(0, "root container")                     // a11y name
enc.label(3, "throughput chart")
enc.label(3, "")                                    // empty clears
enc.place(0, 3, NIL)
enc.cmdSetInputText(2, "seed")
enc.cmdScrollTo(0, 4, 8)
enc.cmdFocus(2)
enc.cmdBlur(2)
enc.place(NIL, 0, NIL)
enc.place(0, 1, NIL)
enc.place(0, 2, NIL)
enc.hidden(1, true)
enc.hidden(1, false)
enc.detach(1)
enc.place(0, 1, NIL)
enc.remove(1)

await Bun.write(new URL("../test/fixture.bin", import.meta.url).pathname, enc.finish(99n))
console.log("wrote fixture.bin")
