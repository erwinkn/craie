# craie

A React-driven native desktop renderer, in Rust. React owns composition;
native code owns a compact retained tree, Taffy flex layout, Parley/Swash
text, an etagere glyph atlas, and a wgpu renderer — two draw calls per
frame.

This is an experiment: does a purpose-built retained host beat translating
React mutations into a general retained-mode framework? See
`docs/ARCHITECTURE.md` and `docs/EXPERIMENTS.md`.

## Run it

```sh
pnpm install   # workspace deps for packages/bridge + examples/js

# native window driven by wire transactions (listens on 127.0.0.1:9470)
cargo run --example app

# React app over the socket (sidebar + messages + composer, ticking)
bun examples/js/demo.tsx           # CRAIE_PORT=9471 to pick a port

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
and presence-masked positional style records — and sends them over a
length-prefixed TCP connection to `bridge::listen` in the native app.

```tsx
import { createRoot, connect, View, Text } from "@craie/bridge"

const root = createRoot(await connect(9470))
root.render(
  <View backgroundColor="#141518" style={{ padding: 32, gap: 8 }}>
    <Text fontSize={20} color="#ececf0">hello</Text>
  </View>
)
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
