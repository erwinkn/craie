# craie

A React-driven native desktop renderer, in Rust. React owns composition;
native code owns a compact retained tree, Taffy flex layout, Parley/Swash
text, an etagere glyph atlas, a wgpu renderer, input/focus/scroll/text
input, and an AccessKit semantic tree.

This is an experiment: does a purpose-built retained host beat translating
React mutations into a general retained-mode framework? See
`docs/ARCHITECTURE.md` and `docs/EXPERIMENTS.md`.

## Run it

```sh
pnpm install        # workspace deps (bridge, react facade, examples)
pnpm build:native   # cargo build -p craie-node + codesign -> craie-node.node

# React apps — esbuild-bundled, run under Node (Bun cannot require
# N-API addons inside worker_threads)
pnpm dev:todo       # todo list: TextInput, ScrollView, persistence
pnpm dev:widgets    # slider behavior + sparkline custom element
pnpm --dir examples/js build && node examples/js/dist/host.mjs  # counter demo

# native window fed by a local submitter thread (no JS needed)
cargo run --example app

# headless renders
cargo run --example text -- --screenshot /tmp/text.png   # direct demo
cargo run --example app -- --screenshot /tmp/app.png     # wire-built UI

# diagnostics + benchmark
cargo run --example sizes
cargo run --release --example bench

# gpui-react-parity frame benchmark: dumps real React commits,
# replays them natively at 100/1k/5k rows
scripts/measure-framebench.sh /tmp/craie-framebench
```

## The bridge

`packages/bridge` (`@craie/bridge`) encodes React commits as binary
transactions — a flat u8-tagged op stream with an interned string table
and presence-masked positional style records — and submits them in-process
through `craie-node` (N-API): the main thread owns the winit event loop
and the retained `Ui`; the React app runs in a `worker_thread` and calls
`NativeClient.submit(bytes)`, which queues one copied transaction and
wakes the loop. Applied seqs and outbound UI events return over one
`subscribe` callback (N-API threadsafe function), so the JS side can
recycle node ids safely and React props like `onPointerDown` fire.

```tsx
// app.tsx (runs in the worker)
import { attachApp, View, Text } from "@craie/react"

const root = attachApp()
root.render(
  <View backgroundColor="#141518" style={{ padding: 32, gap: 8 }}>
    <Text fontSize={20} color="#ececf0">hello</Text>
  </View>
)
```

```ts
// host.ts (main thread)
import { runApp } from "@craie/react"
await runApp(new URL("./app.tsx", import.meta.url),
  { title: "craie", width: 900, height: 640 })
```

### Elements

- `<View>` — flex container, scrollable via `overflow: "scroll"`.
- `<Text>` — text leaf (string child or `text` prop).
- `<TextInput>` — editable text: caret, selection, clipboard, undo, IME.
- `<ScrollView>` — `View` alias for `overflow: "scroll"`.
- `<Custom>` — payload node painted by a `runApp(..., { painters })`
  callback, for content the host doesn't ship.

Event props (`onPointerDown/Move/Up`, `onKeyDown/Up`, `onFocus/Blur`,
`onChangeText`, `onSubmit`, `onScroll`) attach per node; pointer events
carry coordinates relative to the listening node. `focusable` and
`accessibilityLabel` feed focus traversal and the AccessKit tree.

`style` is an RN-ish object (`flexDirection`, `padding`, `gap`,
`alignItems`, `justifyContent`, `width`/`height` as `number | "50%" |
"auto"`, `flexGrow`, …) — see `StyleProps` in `packages/bridge/src/wire.ts`.
Identical styles intern to one native definition.

## Tests

```sh
cargo test                 # host, wire, layout + JS-fixture decode
bun test --cwd packages/bridge   # encoder golden bytes + reconciler ops
```
