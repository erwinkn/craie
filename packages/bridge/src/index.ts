// React entry point: reconciler config + native in-process transport.
//
//   host.ts:   runApp(bindings, new URL("./app.tsx", import.meta.url))
//   app.tsx:   const root = attachApp(bindings); root.render(<View ...>...</View>)

import React, { createContext, createElement, useState, type ReactNode } from "react"
import ReactReconciler from "react-reconciler"
import { ConcurrentRoot, DefaultEventPriority } from "react-reconciler/constants.js"
import { CraieHost, type HostNode, type SurfaceParam, type Transport } from "./host.js"
import {
  SURFACE,
  type AccessibilityRole,
  type ItemDesc,
  type ListTemplate,
  type ScrollAnchor,
  type StyleProps,
} from "./wire.js"

export { attachApp, decodeEvents, loadBindings, runApp, NativeTransport } from "./native.js"
export type { Bindings, NativeClientHandle, NativeHostHandle } from "./native.js"
export {
  ANCHOR,
  Encoder,
  NIL,
  ROLE,
  SURFACE,
  transformMatrix,
  type AccessibilityRole,
  type ItemDesc,
  type ListTemplate,
  type ScrollAnchor,
  type StyleProps,
  type Transform,
  type TransformStep,
} from "./wire.js"
export type { HostNode, SurfaceParam, Transport, UiEvent } from "./host.js"

/** Pointer position + target passed to pointer/wheel listeners. `x`/`y`
 * are window-absolute logical points; `rx`/`ry` are relative to the
 * event target's border box. Wheel listeners get `dx`/`dy` deltas. */
export interface PointerEvt {
  target: HostNode
  x: number
  y: number
  rx?: number
  ry?: number
  button?: number
  dx?: number
  dy?: number
  shift?: boolean
  ctrl?: boolean
  alt?: boolean
  meta?: boolean
}
export interface KeyEvt {
  target: HostNode
  x: number
  y: number
  /** Named-key code; 0 when the press produced text instead. */
  key: number
  /** Printable character for unknown keys. */
  char: string
}
export interface ScrollEvt {
  target: HostNode
  x: number
  y: number
}

export interface ListenerProps {
  onPointerMove?: (e: PointerEvt) => void
  onPointerDown?: (e: PointerEvt) => void
  onPointerUp?: (e: PointerEvt) => void
  onPointerEnter?: (e: PointerEvt) => void
  onPointerLeave?: (e: PointerEvt) => void
  onWheel?: (e: PointerEvt) => void
  onKeyDown?: (e: KeyEvt) => void
  onKeyUp?: (e: KeyEvt) => void
  onFocus?: (e: { target: HostNode }) => void
  onBlur?: (e: { target: HostNode }) => void
  onScroll?: (e: ScrollEvt) => void
}

export interface ViewProps extends ListenerProps {
  style?: StyleProps
  backgroundColor?: string | number
  borderRadius?: number
  borderColor?: string | number
  borderWidth?: number
  /** Participates in Tab traversal. */
  focusable?: boolean
  /** Accessibility name announced by assistive technology. */
  accessibilityLabel?: string
  /** Accessibility role; a plain View has none. */
  accessibilityRole?: AccessibilityRole
  /** Sends `display: none`. */
  hidden?: boolean
  children?: ReactNode
}
export interface PressableProps extends ViewProps {
  /** Primary pointer released over the node. */
  onPress?: (e: PointerEvt) => void
}
export interface TextProps {
  style?: StyleProps
  fontSize?: number
  color?: string | number
  fontWeight?: number | "normal" | "bold"
  fontStyle?: "normal" | "italic"
  /** Family name or generic (`"monospace"`, `"serif"`); the default is
   * `system-ui`. */
  fontFamily?: string
  textDecorationLine?: "none" | "underline" | "line-through" | "underline line-through"
  /** Added to each character's advance, logical points. */
  letterSpacing?: number
  /** Absolute line height, logical points (per paragraph: the outermost
   * Text's). */
  lineHeight?: number
  /** Accessibility name; defaults to the text content. */
  accessibilityLabel?: string
  accessibilityRole?: AccessibilityRole
  hidden?: boolean
  children?: ReactNode // strings land on the wire as text
  text?: string
}
export interface SurfaceProps extends ListenerProps {
  style?: StyleProps
  backgroundColor?: string | number
  borderRadius?: number
  borderColor?: string | number
  borderWidth?: number
  /** Native surface kind (see `SURFACE`); Rust hosts may register more. */
  kind: number
  /** Up to four kind-specific parameters: colors or `{ f32 }` values. */
  params?: SurfaceParam[]
  /** Kind-specific data, copied once per change (identity compare). */
  payload?: ArrayBufferView
  accessibilityLabel?: string
  accessibilityRole?: AccessibilityRole
  hidden?: boolean
}
export interface BarsProps extends Omit<SurfaceProps, "kind" | "params" | "payload"> {
  /** Bar heights in [0, 1]. A new array means new data. */
  values: Float32Array
  color: string | number
  /** Color of the tallest bar; defaults to `color`. */
  maxColor?: string | number
  /** Gap between bars, logical points (default 2). */
  gap?: number
}
export interface TextInputProps extends ListenerProps {
  accessibilityRole?: AccessibilityRole
  style?: StyleProps
  backgroundColor?: string | number
  borderRadius?: number
  borderColor?: string | number
  borderWidth?: number
  fontSize?: number
  color?: string | number
  placeholder?: string
  multiline?: boolean
  focusable?: boolean
  /** Accessibility name announced by assistive technology. */
  accessibilityLabel?: string
  /** Initial text. Inputs are uncontrolled: later `value` changes are
   * ignored; call `setText` on the node ref to replace the text.
   * Native reports edits through `onChangeText`. */
  value?: string
  onChangeText?: (text: string) => void
  onSubmit?: (text: string) => void
  hidden?: boolean
}

export function View(props: ViewProps) {
  return createElement("view", props)
}

/** A View that is a button for assistive technology and fires `onPress`
 * on primary pointer release. */
export function Pressable({ onPress, ...props }: PressableProps) {
  return createElement("view", {
    accessibilityRole: "button",
    focusable: true,
    ...props,
    onPointerUp: (e: PointerEvt) => {
      props.onPointerUp?.(e)
      if ((e.button ?? 1) === 1) onPress?.(e)
    },
  })
}
export function Text(props: TextProps) {
  // Flatten primitive children ("a" {b} "c") into a single `text` prop so
  // mixed string/expression JSX still forms one paragraph. Nested
  // non-primitive children (styled spans) keep their instances.
  const children = props.children
  if (
    props.text === undefined &&
    children !== undefined &&
    flattenText(children) !== undefined
  ) {
    return createElement("text", {
      accessibilityRole: "text",
      ...props,
      text: flattenText(children),
      children: undefined,
    })
  }
  return createElement("text", { accessibilityRole: "text", ...props })
}

/** A native drawing surface fed by payload bytes. */
export function Surface(props: SurfaceProps) {
  return createElement("surface", props)
}

/** Bar chart surface (`SURFACE.bars`). */
export function Bars({ values, color, maxColor, gap, ...props }: BarsProps) {
  return createElement("surface", {
    ...props,
    kind: SURFACE.bars,
    params: [color, maxColor ?? 0, { f32: gap ?? 2 }],
    payload: values,
  })
}

export interface ScrollViewProps extends ViewProps {
  /** Scroll anchoring (default "keep-visible"): the top visible list
   * item keeps its place when extents above it change. "stick-to-end"
   * also holds the end when the view is already there. */
  anchor?: ScrollAnchor
}

export function ScrollView(props: ScrollViewProps) {
  return createElement("view", {
    accessibilityRole: "scrollView",
    ...props,
    style: { overflow: "scroll", ...props.style },
  })
}

export interface ListProps<T> {
  /** The items. Treated as immutable: a changed item is a new object. */
  items: readonly T[]
  /** Stable key of an item's row. */
  keyOf: (item: T, index: number) => string | number
  /** Renders one item's row content. */
  renderItem: (item: T, index: number) => ReactNode
  /** An item's description for native estimates: its row template
   * (index into `templates`) and its text length in characters. Native
   * computes the estimate; rendered rows replace it with their height. */
  describe?: (item: T) => ItemDesc
  /** Row templates: fixed extent, horizontal insets, wrapping font size. */
  templates?: readonly ListTemplate[]
  /** Extent of an item without a template (default 44). */
  estimatedItemSize?: number
  /** Distance rendered beyond the viewport, each side (default 400). */
  overscan?: number
  /** Rows rendered before native reports the first range (default 12). */
  initialCount?: number
  style?: StyleProps
  accessibilityLabel?: string
}

interface Range {
  first: number
  end: number
  /** Key of the item kept rendered for focus. */
  keepKey?: unknown
}

/** A virtualized list. Put it inside a ScrollView: native lays out only
 * the rows in the range it reports (plus a focused row), places them at
 * their item offsets, and anchors the scroll position. */
export function List<T>(props: ListProps<T>) {
  const { items, keyOf, renderItem, initialCount = 12, ...rest } = props
  const [range, setRange] = useState<Range>(() => ({
    first: 0,
    end: Math.min(items.length, initialCount),
  }))
  const onRange = (e: { first: number; end: number; keepKey?: unknown; current: boolean }) =>
    setRange((r) => ({
      // Indices from an older item order are dropped: native reports
      // again for the current one.
      first: e.current ? e.first : r.first,
      end: e.current ? e.end : r.end,
      keepKey: e.keepKey,
    }))
  const row = (i: number) =>
    createElement(
      "view",
      { key: keyOf(items[i]!, i), listIndex: i, accessibilityRole: "listItem" },
      renderItem(items[i]!, i),
    )
  const rows: ReactNode[] = []
  const end = Math.min(range.end, items.length)
  for (let i = range.first; i < end; i++) rows.push(row(i))
  // The focused item by identity, wherever items moved it.
  if (range.keepKey !== undefined) {
    const keep = keyIndex(items, keyOf, range.keepKey)
    if (keep >= 0 && (keep < range.first || keep >= end)) rows.push(row(keep))
  }
  return createElement(
    "list",
    { accessibilityRole: "list", ...rest, items, keyOf, onRange },
    rows,
  )
}

/** Index of the item with `key`, or -1. */
function keyIndex<T>(items: readonly T[], keyOf: (item: T, i: number) => unknown, key: unknown) {
  for (let i = 0; i < items.length; i++) if (keyOf(items[i]!, i) === key) return i
  return -1
}

export function TextInput(props: TextInputProps) {
  // focusable by default; a Tab ring that skips the only editable field
  // would surprise.
  return createElement("input", {
    focusable: true,
    accessibilityRole: props.multiline ? "multilineTextInput" : "textInput",
    ...props,
  })
}

function flattenText(children: ReactNode): string | undefined {
  if (typeof children === "string" || typeof children === "number") return String(children)
  if (Array.isArray(children)) {
    let out = ""
    for (const c of children) {
      if (typeof c === "string" || typeof c === "number") out += c
      else if (c === null || c === undefined || c === false) continue
      else return undefined
    }
    return out
  }
  return undefined
}

// ---------------------------------------------------------------- reconciler

let priority = 0
const context = Object.freeze({})
const noop = () => {}
const no = () => false

const config = {
  rendererVersion: "0.1.0",
  rendererPackageName: "@craie/bridge",
  supportsMutation: true,
  supportsPersistence: false,
  supportsHydration: false,
  isPrimaryRenderer: true,
  supportsMicrotasks: true,
  scheduleMicrotask: queueMicrotask,

  createInstance: (type: string, props: Record<string, any>, root: CraieHost) =>
    root.node(type, props),
  // String children are absorbed into the `text` prop via
  // shouldSetTextContent; createTextInstance covers mixed content.
  createTextInstance: (text: string, root: CraieHost) =>
    root.node("text", { text, accessibilityRole: "text" }),

  appendInitialChild: (parent: HostNode, child: HostNode) => {
    // Parent has no id yet; replayed by materialize.
    parent.initial.push(child)
  },
  appendChild: (parent: HostNode, child: HostNode) =>
    parent.root.place(parent, child, null),
  appendChildToContainer: (root: CraieHost, child: HostNode) =>
    root.place(null, child, null),
  insertBefore: (parent: HostNode, child: HostNode, before: HostNode) =>
    parent.root.place(parent, child, before),
  insertInContainerBefore: (root: CraieHost, child: HostNode, before: HostNode) =>
    root.place(null, child, before),
  // Removal unlinks the subtree root here; every deleted node in the
  // subtree then frees its own slot through detachDeletedInstance.
  removeChild: (parent: HostNode, child: HostNode) => parent.root.detach(child),
  removeChildFromContainer: (root: CraieHost, child: HostNode) => root.detach(child),

  commitUpdate: (n: HostNode, _type: string, oldProps: any, props: any) =>
    n.root.update(n, oldProps, props),
  commitTextUpdate: (n: HostNode, _old: string, text: string) =>
    n.root.setTextContent(n, text),

  // Suspense hiding is `display: none`: the one way to hide.
  hideInstance: (n: HostNode) => n.root.setSuspended(n, true),
  hideTextInstance: (n: HostNode) => n.root.setSuspended(n, true),
  unhideInstance: (n: HostNode) => n.root.setSuspended(n, false),
  unhideTextInstance: (n: HostNode) => n.root.setSuspended(n, false),

  getPublicInstance: (n: HostNode) => n,
  getRootHostContext: () => context,
  getChildHostContext: () => context,
  // String children become a text node's `text` prop. Only the `text`
  // element may absorb them — a View that swallowed string children would
  // render nothing.
  shouldSetTextContent: (type: string, props: any) =>
    type === "text" &&
    (typeof props.children === "string" || typeof props.children === "number"),
  finalizeInitialChildren: () => false,
  prepareForCommit: () => null,
  resetAfterCommit: noop,
  preparePortalMount: noop,
  detachDeletedInstance: (n: HostNode) => n.root.release(n),
  clearContainer: noop,
  commitMount: noop,

  scheduleTimeout: setTimeout,
  cancelTimeout: clearTimeout,
  noTimeout: -1,
  setCurrentUpdatePriority: (v: number) => { priority = v },
  getCurrentUpdatePriority: () => priority,
  resolveUpdatePriority: () => priority || DefaultEventPriority,
  shouldAttemptEagerTransition: no,
  maySuspendCommit: no,
  maySuspendCommitOnUpdate: no,
  maySuspendCommitInSyncRender: no,
  NotPendingTransition: null,
  HostTransitionContext: createContext(null),
  resetFormInstance: noop,
  requestPostPaintCallback: noop,
  trackSchedulerEvent: noop,
  resolveEventType: () => null,
  resolveEventTimeStamp: () => -1,
  preloadInstance: () => true,
  startSuspendingCommit: noop,
  suspendInstance: noop,
  waitForCommitToBeReady: () => null,
  getInstanceFromNode: () => null,
  beforeActiveInstanceBlur: noop,
  afterActiveInstanceBlur: noop,
  prepareScopeUpdate: noop,
  getInstanceFromScope: () => null,
}

// React 19.2's host interface is newer than DefinitelyTyped's reconciler types.
const reconciler: any = ReactReconciler(config as any)
reconciler.injectIntoDevTools?.({
  bundleType: 1,
  version: React.version,
  rendererPackageName: "@craie/bridge",
})

export class Root {
  private container: any
  constructor(readonly host: CraieHost) {
    this.container = reconciler.createContainer(
      host, ConcurrentRoot, null, false, null, "",
      (e: Error) => { throw e }, (e: Error) => console.error(e),
      (e: Error) => console.error(e), null,
    )
  }
  render(node: ReactNode) {
    reconciler.updateContainer(node, this.container, null, null)
  }
  renderSync(node: ReactNode) {
    reconciler.flushSyncFromReconciler(() => this.render(node))
  }
  /** Sends pending ops; resolves when native acks the transaction. */
  flush(): Promise<void> {
    return this.host.flush()
  }
}

export function createRoot(transport: Transport): Root {
  return new Root(new CraieHost(transport))
}
