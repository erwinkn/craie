# craie

A React-driven native desktop renderer, in Rust. React owns composition;
native code owns a compact retained tree, Taffy flex layout, Parley/Swash
text, an etagere glyph atlas, and a wgpu renderer.

This is an experiment: does a purpose-built retained host beat translating
React mutations into a general retained-mode framework? See
`docs/ARCHITECTURE.md` and `docs/EXPERIMENTS.md`.

## Run it

```sh
pnpm install   # workspace deps for packages/bridge + examples/js

# native window fed by a local submitter thread (no JS needed)
cargo run --example app

# React app in-process: native window on the main thread, React in a worker
cargo build -p craie-node && cp target/debug/libcraie_node.dylib craie-node.node
bun examples/js/host.ts

# headless renders
cargo run --example text -- --screenshot /tmp/text.png   # direct demo
cargo run --example app -- --screenshot /tmp/app.png     # wire-built UI

# diagnostics + benchmark
cargo run --example sizes
cargo run --release --example bench
```

## The bridge

`packages/bridge` (`@craie/bridge`) encodes React commits as binary
transactions — a flat u8-tagged op stream with an interned string table
and presence-masked positional style records — and submits them in-process
through `craie-node` (N-API): the main thread owns the winit event loop
and the retained `Ui`; the React app runs in a `worker_thread` and calls
`NativeClient.submit(bytes)`, which queues one copied transaction and
wakes the loop. Applied seqs are acked back so the JS side can recycle
node ids safely.

```tsx
// app.tsx (runs in the worker)
import { attachApp, loadBindings, View, Text } from "@craie/bridge"

const root = attachApp(loadBindings())
root.render(
  <View backgroundColor="#141518" style={{ padding: 32, gap: 8 }}>
    <Text fontSize={20} color="#ececf0">hello</Text>
  </View>
)
```

```ts
// host.ts (main thread)
import { loadBindings, runApp } from "@craie/bridge"
await runApp(loadBindings(), new URL("./app.tsx", import.meta.url),
  { title: "craie", width: 900, height: 640 })
```

### Elements

- `<View>` — flex container. Props: `style`, `backgroundColor`,
  `hidden`, `children`.
- `<Text>` — text leaf. Props: `text` (or a string child), `fontSize`,
  `color`, `style`, `hidden`.

`style` is an RN-ish object (`flexDirection`, `padding`, `gap`,
`alignItems`, `justifyContent`, `width`/`height` as `number | "50%" |
"auto"`, `flexGrow`, …) — see `StyleProps` in `packages/bridge/src/wire.ts`.
Identical styles intern to one native definition.

## Tests

```sh
cargo test                 # host, wire, layout + JS-fixture decode
bun test --cwd packages/bridge   # encoder golden bytes + reconciler ops
```
