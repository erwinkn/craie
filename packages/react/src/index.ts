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
  type Root,
} from "@craie/bridge"

export {
  View,
  Pressable,
  Text,
  TextInput,
  ScrollView,
  Layer,
  FocusTrap,
  FocusGroup,
  List,
  Surface,
  Bars,
  Vector,
  Image,
  Portal,
  defineStates,
  Path,
  Circle,
  Ellipse,
  Rect,
  Line,
  Polyline,
  Polygon,
  G,
  useFrameStats,
  onFrameStats,
  createRoot,
  loadBindings,
  NativeTransport,
  decodeEvents,
  Encoder,
  NIL,
  ROLE,
  SURFACE,
  ANCHOR,
  FIT,
} from "@craie/bridge"
export type {
  ListProps,
  ScrollViewProps,
  LayerProps,
  FocusTrapProps,
  FocusGroupProps,
  ListTemplate,
  ItemDesc,
  ScrollAnchor,
  ViewProps,
  PressableProps,
  StateProps,
  Variants,
  VariantStyle,
  TextProps,
  TextInputProps,
  SurfaceProps,
  BarsProps,
  VectorProps,
  ImageProps,
  ImageFit,
  ImageLoadEvt,
  ImageErrorEvt,
  ShapeProps,
  PathProps,
  CircleProps,
  EllipseProps,
  RectProps,
  LineProps,
  PolyProps,
  GProps,
  FrameStats,
  Timing,
  Transitions,
  SurfaceParam,
  ListenerProps,
  PointerEvt,
  KeyEvt,
  ScrollEvt,
  HostNode,
  Transport,
  UiEvent,
  StyleProps,
  Transform,
  TransformStep,
  AccessibilityRole,
  Bindings,
  NativeClientHandle,
  NativeHostHandle,
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
  } = {},
  bindings?: Bindings,
): Promise<void> {
  return run(bindings ?? loadBindings(), entry, options)
}
