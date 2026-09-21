// Public React API for Craie. React composes the application; the
// native host executes interaction and rendering. This package is the
// stable surface — `@craie/bridge` is the wire/reconciler machinery.
//
//   host.ts:  await runApp(new URL("./app.tsx", import.meta.url), { title })
//   app.tsx:  const root = attachApp(); root.render(<App />)

import {
  attachApp as attach,
  loadBindings,
  runApp as run,
  type Bindings,
  type PainterFn,
  type Root,
} from "@craie/bridge"

export {
  View,
  Text,
  TextInput,
  ScrollView,
  Custom,
  createRoot,
  loadBindings,
  NativeTransport,
  decodeEvents,
  Encoder,
  NIL,
} from "@craie/bridge"
export type {
  ViewProps,
  TextProps,
  TextInputProps,
  CustomProps,
  ListenerProps,
  PointerEvt,
  KeyEvt,
  ScrollEvt,
  HostNode,
  Transport,
  UiEvent,
  StyleProps,
  Bindings,
  NativeClientHandle,
  NativeHostHandle,
  PaintQuad,
  PaintSpec,
  PainterFn,
  Root,
} from "@craie/bridge"

/** Worker side: attach to the host session (loads the addon itself) and
 * return a ready root. */
export function attachApp(bindings?: Bindings): Root {
  return attach(bindings ?? loadBindings())
}

/** Main thread: open the native window, spawn the app worker, run the
 * platform event loop until the window closes. */
export function runApp(
  entry: string | URL,
  options: {
    title?: string
    width?: number
    height?: number
    painters?: Record<number, PainterFn>
  } = {},
  bindings?: Bindings,
): Promise<void> {
  return run(bindings ?? loadBindings(), entry, options)
}
