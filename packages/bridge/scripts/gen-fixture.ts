// Generates test/fixture.bin: one CRW2 transaction covering every op the
// Rust decoder must accept. Regenerate after wire format changes:
//   bun packages/bridge/scripts/gen-fixture.ts
import {
  CHORD_FLAG, CLAIM_KIND, CURRENT, ENV_BIT, EVENT_MASK, Encoder, FIT, NIL, PRESS_FLAG, REPORTED, ROLE, STATE_BIT, SUBMIT_KEY, SURFACE,
  parseChord, transformMatrix,
} from "../src/wire.js"

const enc = new Encoder()
enc.create(0, 0)                                    // view
enc.create(1, 1)                                    // text
const text = "héllo — مرحبا 日本語"
const bold = new TextEncoder().encode("héllo ").length // byte offset of span 1
enc.paragraph(1, text, [
  { start: 0, fontSize: 18.5, color: 0x6dc7_ff80, lineHeight: 24, inheritColor: true },
  {
    start: bold, fontSize: 18.5, color: 0xffff_ffff, weight: 700, italic: true,
    fontFamily: "monospace", decoration: 3, letterSpacing: 0.5, pressable: true,
  },
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
enc.spatial(0, transformMatrix([{ translateX: 3 }, { scale: 2 }]), 0.75, -2)
enc.paint(0, 0x1122_33ff, 6.5, { color: 0xff00_00ff, width: 2 })
enc.create(2, 2)                                    // input
enc.inputConfig(2, 15, "type here", true, SUBMIT_KEY["mod+enter"])
enc.interaction(2, 0x7ff, true)                     // all listeners, focusable
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
enc.cmdInsertText(2, "!")                           // replaces the selection
enc.cmdBlur(2)
enc.cmdWriteClipboard(NIL, "copied")
enc.claims(2, 7, [                                  // claim sets
  parseChord("mod+shift+k", false)!,
  { ...parseChord("escape", false)!, flags: CHORD_FLAG.named | CHORD_FLAG.noRepeat },
  { kind: CLAIM_KIND.paste, flags: 0, mods: 0, key: 0 },
])
enc.claims(NIL, 3, [{ ...parseChord("shift+?", false)!, flags: CHORD_FLAG.inInput }])
enc.detach(1)
enc.place(0, 1, NIL)
enc.remove(1)
enc.create(4, 4)                                    // list
enc.listConfig(4, 250, 36, [{ base: 12, inset: 16, fontSize: 14 }, { base: 48 }])
enc.listSplice(4, 0, 0, [{ template: 0, textLength: 42, id: 5 }, { template: 1, id: 6 }, { textLength: 70000, id: 7 }])
enc.listSplice(4, 1, 1, [])                         // removes id 6
enc.listSplice(4, 0, 2, [{ textLength: 70000, id: 7, unchanged: true }, { template: 0, textLength: 42, id: 5, unchanged: true }])
enc.place(0, 4, NIL)
enc.create(5, 0)                                    // a row
enc.listIndex(5, 1)
enc.place(4, 5, NIL)
enc.scrollAnchor(0, "stick-to-end")
enc.transition(0, {                                 // animation family
  opacity: { duration: 250, delay: 50, easing: "ease-out" },
  width: { spring: { stiffness: 200, damping: 20 } },
})
enc.animate(0, "backgroundColor", [0xff00_00ff], { duration: 300, easing: [0.1, 0.2, 0.3, 0.4] })
enc.animate(0, "gap", [4, 6], { spring: {}, delay: 20 })
enc.states(0, (1n << BigInt(STATE_BIT.selected)) | 1n) // state styles
enc.role(0, ROLE.switch, REPORTED.expanded | REPORTED.selected)
enc.interaction(0, EVENT_MASK.press | EVENT_MASK.activate, false, false,
  PRESS_FLAG.pressable | PRESS_FLAG.keepFocus)       // a pressable keeping focus
enc.variants(5, [                                  // on the row, scoped by the root
  { terms: [{ scope: 0, mask: 1n << BigInt(STATE_BIT.selected) }], env: 0, values: { fill: 0x2d32_40ff, color: 0xffff_ffff } },
  {
    terms: [{ scope: 0, mask: 1n }], env: ENV_BIT.narrow,
    values: {
      borderColor: 0x0000_00ff, borderWidth: 1, radius: 3, color: null, opacity: 0.5,
      transform: [1, 0, 0, 1, 0, 2], layout: { width: "50%", height: 44 },
    },
  },
])
enc.environment(900, 500)
enc.color(0, 0x9aa0_aaff)
enc.color(2, null)
enc.animate(0, "color", [0xffff_ffff], { duration: 100 })

// A vector node and a minimal asset (CRV1: a 10 x 10 view box, one
// solid paint, one nonzero fill of a triangle), written by hand here;
// crates/vector/src/asset.rs is the format's definition.
const asset = (() => {
  const b: number[] = []
  const u8 = (v: number) => b.push(v & 0xff)
  const u16 = (v: number) => { u8(v); u8(v >> 8) }
  const u32 = (v: number) => { u16(v & 0xffff); u16(v >>> 16) }
  const f32 = (v: number) => { const x = new DataView(new ArrayBuffer(4)); x.setFloat32(0, v, true); for (let i = 0; i < 4; i++) u8(x.getUint8(i)) }
  u32(0x3156_5243); u16(1); u16(0)            // "CRV1", version 1, flags
  for (const v of [0, 0, 10, 10]) f32(v)       // view box
  u32(1); u32(1); u32(4); u32(3)               // paints, items, verbs, points
  u8(0); u32(0x0080_ffff)                      // solid paint
  u8(0); u32(0); f32(1)                        // fill item, paint 0, opacity 1
  for (const v of [1, 0, 0, 1, 0, 0]) f32(v)   // transform
  u32(0); u32(4); u32(0); u8(1)                // verbs 0..4 from point 0, even-odd
  for (const v of [0, 1, 1, 4]) u8(v)          // move, line, line, close
  for (const v of [0, 0, 10, 0, 5, 10]) f32(v) // points
  return new Uint8Array(b)
})()
enc.create(6, 5)                                    // vector
enc.payload(6, asset)
enc.place(0, 6, NIL)
enc.create(7, 0)                                    // a layer container
enc.layer(7, 6)                                     // owned from the vector
enc.role(7, ROLE.radiogroup)                        // the last role
enc.spatial(7, undefined, undefined, 50)            // z alone
enc.place(NIL, 7, NIL)
// A runtime drawing: a dashed arc path and an even-odd polygon filled
// with the inherited color at half alpha (currentColor).
const shape = {
  kind: 0, geometry: "M2 12a10 10 0 0 1 20 0", transform: "", dashes: "4 2",
  fill: 0, fillRule: 0, stroke: 0x1122_33ff, current: 0, strokeWidth: 2, join: 1, cap: 2,
  miterLimit: 4, dashOffset: 1.5, opacity: 1,
}
enc.create(8, 5)                                    // vector
enc.drawing(8, "0 0 24 24", [
  shape,
  { ...shape, kind: 2, geometry: "4,4 20,4 12,20", transform: "rotate(90 12 12)", dashes: "", fill: 0xffff_ff80, current: CURRENT.fill, fillRule: 1, stroke: 0, opacity: 0.5 },
])
enc.place(0, 8, NIL)
// An image node: encoded bytes (native decodes them later, off the UI
// thread: any bytes are accepted here) and its fit.
enc.create(9, 7)                                    // image
enc.payload(9, new Uint8Array([0x89, 0x50, 0x4e, 0x47, 1, 2, 3]))
enc.imageConfig(9, FIT.contain)
enc.place(0, 9, NIL)

await Bun.write(new URL("../test/fixture.bin", import.meta.url).pathname, enc.finish(99n))
console.log("wrote fixture.bin")
