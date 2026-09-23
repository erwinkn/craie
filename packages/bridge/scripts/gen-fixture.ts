// Generates test/fixture.bin: one CRW2 transaction covering every op the
// Rust decoder must accept. Regenerate after wire format changes:
//   bun packages/bridge/scripts/gen-fixture.ts
import { Encoder, NIL, ROLE, SURFACE, transformMatrix } from "../src/wire.js"

const enc = new Encoder()
enc.create(0, 0)                                    // view
enc.create(1, 1)                                    // text
const text = "héllo — مرحبا 日本語"
const bold = new TextEncoder().encode("héllo ").length // byte offset of span 1
enc.paragraph(1, text, [
  { start: 0, fontSize: 18.5, color: 0x6dc7_ff80 },
  { start: bold, fontSize: 18.5, color: 0xffff_ffff, weight: 700, italic: true },
])
enc.layout(0, {
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
enc.spatial(0, transformMatrix([{ translateX: 3 }, { scale: 2 }]), 0.75)
enc.paint(0, 0x1122_33ff, 6.5, { color: 0xff00_00ff, width: 2 })
enc.create(2, 2)                                    // input
enc.inputConfig(2, 15, 0xffff_ffff, "type here", true)
enc.interaction(2, 0x1ff, true)                     // all listeners, focusable
enc.role(2, ROLE.multilineTextInput)
enc.create(3, 3)                                    // surface
enc.surface(3, SURFACE.bars, [0x6dc7_c8ff, 0x6dc7_ffff, 0, 0])
enc.payload(3, new Float32Array([0.25, 0.5, 0.75, 1.0]))
enc.paint(3, 0x1b1d_24ff, 4, undefined)
enc.label(0, "root container")                      // a11y name
enc.label(3, "throughput chart")
enc.label(3, "")                                    // empty clears
enc.place(NIL, 0, NIL)
enc.place(0, 3, NIL)
enc.place(0, 1, 3)                                  // before the surface
enc.place(0, 2, NIL)
enc.cmdSetText(2, "seed")
enc.cmdScrollTo(0, 4, 8)
enc.cmdFocus(2)
enc.cmdBlur(2)
enc.detach(1)
enc.place(0, 1, NIL)
enc.remove(1)
enc.create(4, 4)                                    // list
enc.listConfig(4, 250, 36, [{ base: 12, inset: 16, fontSize: 14 }, { base: 48 }])
enc.listSplice(4, 0, 0, [{ template: 0, textLength: 42 }, { template: 1 }, { textLength: 70000 }])
enc.listSplice(4, 1, 1, [])
enc.place(0, 4, NIL)
enc.create(5, 0)                                    // a row
enc.listIndex(5, 1)
enc.place(4, 5, NIL)
enc.scrollAnchor(0, "stick-to-end")

await Bun.write(new URL("../test/fixture.bin", import.meta.url).pathname, enc.finish(99n))
console.log("wrote fixture.bin")
