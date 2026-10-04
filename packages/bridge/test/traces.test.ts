import { test, expect } from "bun:test"
import { readdirSync, readFileSync } from "node:fs"

// The shared list traces (docs/contracts/list-traces.md) ship with this
// package. Both list implementations run them; this only checks that each
// file says nothing the format doesn't define, so a typo can't make a step
// check less than it seems to.
const DIR = new URL("../traces/lists/", import.meta.url)
const TOP = ["format", "id", "name", "viewport", "config", "templates", "items", "steps"]
const CONFIG = ["overscan", "lookahead", "retain", "endThreshold", "anchor", "anchorPolicy", "startInset", "paddingEnd"]
const BLOCK = ["count", "key", "version", "loaded", "failed", "estimate", "height"]
const STEPS: Record<string, string[]> = {
  mount: [], scroll: ["offset"], scrollBy: ["delta"], scrollToIndex: ["index", "align"],
  scrollToKey: ["key", "align"], scrollToEnd: [], scrollToOffset: ["offset"], changes: ["changes"],
  measure: ["key", "height"], resize: ["width", "height"], focus: ["key"], blur: ["key"],
  followKey: ["value"],
}
const OPS: Record<string, string[]> = {
  splice: ["kind", "at", "remove", "items"], move: ["kind", "from", "count", "to"], update: ["kind", "at", "items"],
}
const EXPECT = ["visible", "anchor", "offset", "atEnd", "following", "pinnedKeys", "mounted", "load", "unload", "held"]

const only = (what: string, o: object, keys: string[]) =>
  expect(Object.keys(o).filter(k => !keys.includes(k)), what).toEqual([])

const files = readdirSync(DIR).filter(f => f.endsWith(".json")).sort()

test("the trace set is the contract's", () => {
  expect(files.map(f => f.split("-")[0])).toEqual([
    "I1", "I2", "I3", "I3j", "I4", "I4b", "I4bj", "I4j",
    "K1", "K10", "K11", "K12", "K13", "K14", "K15", "K16", "K17", "K18", "K2", "K3", "K4", "K5", "K6", "K7", "K8", "K9",
    "L1", "L2", "L3", "L4", "L5", "L6", "L7", "U1", "U2", "U3", "U4",
  ])
})

for (const file of files) {
  test(`${file} follows craie-list-trace/1`, () => {
    const t = JSON.parse(readFileSync(new URL(file, DIR), "utf8"))
    only("top level", t, TOP)
    expect(t.format).toBe("craie-list-trace/1")
    expect(file.startsWith(`${t.id}-`)).toBe(true)
    only("config", t.config ?? {}, CONFIG)
    for (const block of t.items) only("item block", block, BLOCK)
    for (const step of t.steps) {
      const fields = STEPS[step.do]
      expect(fields, `step ${step.do}`).toBeDefined()
      only(`${step.do} step`, step, ["do", "expect", ...fields])
      only(`${step.do} expect`, step.expect ?? {}, EXPECT)
      for (const op of step.changes ?? []) {
        const fields = OPS[op.kind]
        expect(fields, `op ${op.kind}`).toBeDefined()
        only(`${op.kind} op`, op, fields)
        for (const block of op.items ?? []) only(`${op.kind} block`, block, BLOCK)
      }
    }
  })
}
