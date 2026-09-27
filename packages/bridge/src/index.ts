// React entry point: reconciler config + native in-process transport.
//
//   host.ts:   runApp(bindings, new URL("./app.tsx", import.meta.url))
//   app.tsx:   const root = attachApp(bindings); root.render(<View ...>...</View>)

import React, {
  createContext,
  createElement,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
  type ReactNode,
  type Ref,
} from "react"
import ReactReconciler from "react-reconciler"
import {
  ConcurrentRoot,
  ContinuousEventPriority,
  DefaultEventPriority,
  DiscreteEventPriority,
} from "react-reconciler/constants.js"
import {
  CraieHost,
  onFrameStats as onFrameStatsInternal,
  type ClipboardEvt,
  type ContextMenuEvt,
  type DropEvt,
  type FrameStats as FrameStatsReport,
  type Hotkey,
  type HostNode,
  type KeyClaim,
  type ScopeChain,
  type ScopeRef,
  type SurfaceParam,
  type Transport,
} from "./host.js"
import { flattenShapes, type ShapeProps } from "./shapes.js"
import {
  EVENT_KIND,
  SURFACE,
  type AccessibilityRole,
  type AnimProp,
  type AnimationEnd,
  type Easing,
  type EndReason,
  type ItemDesc,
  type ListTemplate,
  type ScrollAnchor,
  type StyleProps,
  type SubmitKey,
} from "./wire.js"

export { attachApp, decodeEvents, loadBindings, runApp, NativeTransport } from "./native.js"
export type { Bindings, NativeClientHandle, NativeHostHandle } from "./native.js"
export {
  ANCHOR,
  ANIM_PROP,
  EASING,
  END_REASON,
  Encoder,
  NIL,
  ROLE,
  SURFACE,
  parseChord,
  transformMatrix,
  type AccessibilityRole,
  type ItemDesc,
  type ListTemplate,
  type ScrollAnchor,
  type StyleProps,
  type SubmitKey,
  type Timing,
  type Transform,
  type TransformStep,
  type Transitions,
} from "./wire.js"
export type {
  ClipboardEvt,
  ContextMenuEvt,
  DropEvt,
  FrameStats,
  Hotkey,
  HostNode,
  KeyClaim,
  SurfaceParam,
  Transport,
  UiEvent,
} from "./host.js"
export { defineStates, onFrameStats } from "./host.js"
export { Circle, Ellipse, G, Line, Path, Polygon, Polyline, Rect } from "./shapes.js"
export type {
  CircleProps, EllipseProps, GProps, LineProps, PathProps, PolyProps, RectProps, ShapeProps,
} from "./shapes.js"

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
  /** Named-key code (wire.ts `KEY_CODE`); 0 for a character key. */
  key: number
  /** The character the key gives on the current layout, Shift and Alt
   * applied, Ctrl and Cmd not (the web's `event.key`): "O" for Shift+O. */
  char: string
  shift: boolean
  ctrl: boolean
  alt: boolean
  meta: boolean
  /** The platform's auto-repeat of a held key. */
  repeat: boolean
  /** An IME composes: the key belongs to it. */
  composing: boolean
  /** The physical key as the web's `event.code`: "KeyC" for the C
   * position on any layout, "Digit1", "Slash", "Enter", "F5"; "" for
   * keys Craie doesn't name. */
  code: string
}
export interface ScrollEvt {
  target: HostNode
  x: number
  y: number
}

export interface ListenerProps {
  /** The native node: `focus`, `blur`, `scrollTo`, `setText`,
   * `animate`. */
  ref?: Ref<HostNode>
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
  /** Keys this node claims while it or a descendant has focus: native
   * skips its own handling and runs the first matching claim. */
  keymap?: KeyClaim[]
  /** Claims paste (focus inside this node): native does not insert, and
   * the returned text replaces the input's selection. */
  onPaste?: (e: ClipboardEvt) => string | void
  /** Claims copy of the focused input's or the selection's text: the
   * returned text goes to the clipboard. */
  onCopy?: (e: ClipboardEvt) => string | void
  /** Claims cut: the returned text goes to the clipboard, and the
   * input's selection is deleted; nothing happens without a return. */
  onCut?: (e: ClipboardEvt) => string | void
  /** Claims file drops under the pointer. */
  onDrop?: (e: DropEvt) => void
  /** Claims context-menu requests: a secondary press here, or the
   * ContextMenu key or Shift+F10 with focus inside. */
  onContextMenu?: (e: ContextMenuEvt) => void
}

/** What a variant overrides. */
export interface VariantStyle {
  backgroundColor?: string | number
  borderColor?: string | number
  borderWidth?: number
  borderRadius?: number
  /** The color text inherits. */
  color?: string | number
  /** Layout, `opacity` and `transform`. Keys that share a wire field
   * with ones the variant sets (`width` with `height`, the sides of
   * `padding`) keep the element's own values. */
  style?: StyleProps
}
/** State and environment variants, resolved natively (no render): a
 * key names a state of the nearest scope (`_hover`, `_selected`, a
 * `defineStates` state), an environment condition (`_narrow`,
 * `_compact`, `_touch`, `_reducedMotion`), or a `group` up the tree
 * (`_row`, whose states the block then reads). Nesting ANDs; of the
 * variants that hold, the more specific one wins per property. */
export type Variants = { [key: `_${string}`]: (VariantStyle & Variants) | undefined }

/** The states a scope sets (hover, press and focus are native's). */
export interface StateProps {
  selected?: boolean
  expanded?: boolean
  checked?: boolean
  highlighted?: boolean
  /** Also masks hover and press, and stops `onPress`. */
  disabled?: boolean
  /** Custom states (`defineStates`) by name. */
  states?: Record<string, boolean>
}

export interface ViewProps extends ListenerProps, StateProps, Variants {
  style?: StyleProps
  /** Makes the View a scope whose states its variants and its
   * descendants' read; a name also addresses it (`_name`) from further
   * down. Adding or removing it remounts the children. */
  group?: boolean | string
  /** The color descendant text inherits. */
  color?: string | number
  /** The View's text descendants form one selection domain: drag to
   * select across them, Cmd/Ctrl+C copies in tree order. */
  selectable?: boolean
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
/** Text props. A Text nested in a Text has no native node: its text and
 * style become spans of the outermost Text's paragraph, and its pointer
 * listeners (`onPress` too) receive the events over its own span. */
export interface TextProps extends ListenerProps, Variants {
  style?: StyleProps
  /** Primary pointer released over this text (or this nested span). */
  onPress?: (e: PointerEvt) => void
  /** This Text alone is a selection domain (on the outermost Text). */
  selectable?: boolean
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
export interface SurfaceProps extends ListenerProps, Variants {
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
export interface TextInputProps extends ListenerProps, Variants {
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
  /** Enter submits (per `submitKey`) only when set; without it, Enter
   * in a multiline input is a newline. */
  onSubmit?: (text: string) => void
  /** Which Enter submits: `enter` (default; Shift+Enter too when single
   * line) or `mod+enter`. Other Enters in a multiline input insert a
   * newline. */
  submitKey?: SubmitKey
  hidden?: boolean
}

const HostContext = createContext<CraieHost | null>(null)
const ScopeContext = createContext<ScopeChain | null>(null)

/** A host element with the scope chain its variants read. A scope
 * heads the chain it and its children see; children get it through
 * context, so a Portal's content keeps its owner's scopes. The
 * Provider is there even when the element is no scope, so toggling
 * `group` keeps the children mounted. */
function useHost(type: string, props: Record<string, any>, scope = false) {
  const outer = useContext(ScopeContext)
  const [ref] = useState<ScopeRef>(() => ({ node: null }))
  const name = typeof props.group === "string" ? props.group : undefined
  const chain = useMemo(
    () => (scope ? { ref, name, parent: outer } : outer),
    [scope, ref, name, outer],
  )
  const own = scope ? { __scope: ref, __scopes: chain } : outer ? { __scopes: outer } : {}
  if (props.children === undefined) return createElement(type, { ...props, ...own })
  return createElement(
    type,
    { ...props, ...own },
    createElement(ScopeContext.Provider, { value: chain }, props.children),
  )
}

/** Renders `children` as a window root, above the app (overlays,
 * menus, tooltips). They keep the context of where the Portal sits,
 * scopes included. */
export function Portal({ children }: { children?: ReactNode }) {
  const host = useContext(HostContext)
  return host ? reconciler.createPortal(children, host, null, null) : null
}

/** Window-level shortcuts, matched after every claim on the focus path
 * and, unless `allowInInput`, not while a text input has focus. The
 * window list is every mounted hook's bindings in mount order; the first
 * match wins. */
export function useHotkeys(bindings: readonly Hotkey[]) {
  const host = useContext(HostContext)
  const [owner] = useState(() => ({}))
  useLayoutEffect(() => {
    host?.setHotkeys(owner, bindings)
  })
  useLayoutEffect(() => () => host?.setHotkeys(owner, null), [host, owner])
}

/** Writes plain text to the system clipboard. */
export function useClipboard(): { write(text: string): void } {
  const host = useContext(HostContext)
  return { write: (text: string) => host?.writeClipboard(text) }
}

/** The latest native frame statistics (`null` until the first report).
 * Reports come only while frames are drawn. */
export function useFrameStats(): FrameStatsReport | null {
  const [stats, setStats] = useState<FrameStatsReport | null>(null)
  useEffect(() => onFrameStatsInternal(setStats), [])
  return stats
}

export function View(props: ViewProps) {
  return useHost("view", props, !!props.group)
}

/** A View that is a button for assistive technology and fires `onPress`
 * on primary pointer release. Always a scope. */
export function Pressable({ onPress, ...props }: PressableProps) {
  return useHost("view", {
    accessibilityRole: "button",
    ...props,
    focusable: !props.disabled && (props.focusable ?? true),
    onPointerUp: (e: PointerEvt) => {
      props.onPointerUp?.(e)
      if ((e.button ?? 1) === 1 && !props.disabled) onPress?.(e)
    },
  }, true)
}
export function Text({ onPress, ...rest }: TextProps) {
  const props: TextProps = onPress
    ? {
        ...rest,
        onPointerUp: (e: PointerEvt) => {
          rest.onPointerUp?.(e)
          if ((e.button ?? 1) === 1) onPress(e)
        },
      }
    : rest
  // Flatten primitive children ("a" {b} "c") into a single `text` prop so
  // mixed string/expression JSX still forms one paragraph. Nested
  // non-primitive children (styled spans) keep their instances.
  const children = props.children
  const flat = props.text === undefined && children !== undefined ? flattenText(children) : undefined
  if (flat !== undefined) {
    return useHost("text", { accessibilityRole: "text", ...props, text: flat, children: undefined })
  }
  return useHost("text", { accessibilityRole: "text", ...props })
}

/** A native drawing surface fed by payload bytes. */
export function Surface(props: SurfaceProps) {
  return useHost("surface", props)
}

interface VectorBase extends ListenerProps, Variants {
  style?: StyleProps
  backgroundColor?: string | number
  borderRadius?: number
  borderColor?: string | number
  borderWidth?: number
  accessibilityLabel?: string
  accessibilityRole?: AccessibilityRole
  hidden?: boolean
}

/** A prepared asset: the bytes `craie-svg in.svg out.crv` writes (SVG
 * documents import at build time). Its paint is baked in. */
export interface VectorAssetProps extends VectorBase {
  asset: Uint8Array
  viewBox?: undefined
  children?: undefined
}

/** Runtime shapes: `Path`, `Circle`, ... children in a view box. The
 * paint props (`fill`, `stroke`, ...) are defaults the shapes inherit;
 * `opacity` is the node's (as `style.opacity`: the drawing fades as one
 * layer, and animates). */
export interface VectorShapeProps extends VectorBase, ShapeProps {
  asset?: undefined
  /** "minX minY width height". Empty or zero-size draws nothing. */
  viewBox: string
  children?: ReactNode
}

/** A Vector with neither draws nothing. */
export interface VectorEmptyProps extends VectorBase {
  asset?: undefined
  viewBox?: undefined
  children?: undefined
}

export type VectorProps = VectorAssetProps | VectorShapeProps | VectorEmptyProps

/** A vector drawing (icons, illustrations, charts). Its view box is the
 * node's intrinsic size; the drawing fits its content box, centered,
 * aspect kept. An image for assistive technology unless a role is
 * given.
 *
 *   <Vector viewBox="0 0 24 24" fill="none" stroke="#fff" strokeWidth={2}>
 *     <Circle cx={12} cy={12} r={10} />
 *     <Path d="m9 12 2 2 4-4" />
 *   </Vector>
 */
export function Vector(props: VectorProps) {
  if (props.viewBox === undefined) {
    const { children: _, ...rest } = props
    return useHost("vector", { accessibilityRole: "image", ...rest })
  }
  const {
    children, viewBox, asset: _, fill, fillOpacity, fillRule, stroke, strokeOpacity,
    strokeWidth, strokeLinecap, strokeLinejoin, strokeMiterlimit, strokeDasharray,
    strokeDashoffset, color, opacity, transform, ...rest
  } = props as VectorShapeProps
  const shapes = flattenShapes(children, {
    fill, fillOpacity, fillRule, stroke, strokeOpacity, strokeWidth, strokeLinecap,
    strokeLinejoin, strokeMiterlimit, strokeDasharray, strokeDashoffset, color, transform,
  }, viewBox)
  if (opacity !== undefined) {
    const o = Math.min(Math.max(Number(opacity), 0), 1)
    rest.style = { ...rest.style, opacity: (rest.style?.opacity ?? 1) * (Number.isNaN(o) ? 1 : o) }
  }
  return useHost("vector", { accessibilityRole: "image", ...rest, viewBox, shapes })
}

/** Bar chart surface (`SURFACE.bars`). */
export function Bars({ values, color, maxColor, gap, ...props }: BarsProps) {
  return useHost("surface", {
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
  return useHost("view", {
    accessibilityRole: "scrollView",
    ...props,
    style: { overflow: "scroll", ...props.style },
  }, !!props.group)
}

/** The layer a subtree renders in (null: the app itself). */
const LayerOwner = createContext<HostNode | null>(null)

export interface LayerProps {
  /** Order among layers (kit tokens: dropdown 50, modal 70, toast 80...). */
  z?: number
  children?: ReactNode
}

/** Renders `children` in a layer: a container filling the window at the
 * root level, above the app. Layers order by `z`, then by the order they
 * opened, and never below the layer they were opened from:
 *
 *   <Layer z={70}><Dialog>   // opened from the app
 *     <Layer z={50}><Menu /></Layer>   // paints above the dialog
 *   </Dialog></Layer>
 *
 * The container is transparent to hit testing: a press where its
 * children are not reaches whatever is below. */
export function Layer({ z = 0, children }: LayerProps) {
  const host = useContext(HostContext)
  const owner = useContext(LayerOwner)
  if (!host) throw Error("Layer outside a Craie root")
  const [container] = useState(() => host.layer(owner, z))
  useLayoutEffect(() => {
    const old = container.props
    if (old.style.zIndex === z) return
    const props = { style: { ...old.style, zIndex: z } }
    if (container.mounted) host.update(container, old, props)
    else container.props = props
  }, [z])
  return reconciler.createPortal(
    createElement(LayerOwner.Provider, { value: container }, children),
    container, null, null,
  )
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
  return useHost("input", {
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

/** A container is the root, or a layer container (`Layer`'s portal). */
type Container = CraieHost | HostNode
const hostOf = (c: Container) => (c instanceof CraieHost ? c : c.root)

const config = {
  rendererVersion: "0.1.0",
  rendererPackageName: "@craie/bridge",
  supportsMutation: true,
  supportsPersistence: false,
  supportsHydration: false,
  isPrimaryRenderer: true,
  supportsMicrotasks: true,
  scheduleMicrotask: queueMicrotask,

  createInstance: (type: string, props: Record<string, any>, c: Container) =>
    hostOf(c).node(type, props),
  // String children are absorbed into the `text` prop via
  // shouldSetTextContent; createTextInstance covers mixed content.
  createTextInstance: (text: string, c: Container) =>
    hostOf(c).node("text", { text, accessibilityRole: "text" }),

  appendInitialChild: (parent: HostNode, child: HostNode) => {
    // Parent has no id yet; replayed by materialize.
    parent.initial.push(child)
  },
  appendChild: (parent: HostNode, child: HostNode) =>
    parent.root.place(parent, child, null),
  appendChildToContainer: (c: Container, child: HostNode) =>
    c instanceof CraieHost ? c.place(null, child, null) : c.root.placeInLayer(c, child, null),
  insertBefore: (parent: HostNode, child: HostNode, before: HostNode) =>
    parent.root.place(parent, child, before),
  insertInContainerBefore: (c: Container, child: HostNode, before: HostNode) =>
    c instanceof CraieHost ? c.place(null, child, before) : c.root.placeInLayer(c, child, before),
  // Removal unlinks the subtree root here; every deleted node in the
  // subtree then frees its own slot through detachDeletedInstance.
  removeChild: (parent: HostNode, child: HostNode) => parent.root.detach(child),
  removeChildFromContainer: (c: Container, child: HostNode) =>
    c instanceof CraieHost ? c.detach(child) : c.root.removeFromLayer(c, child),

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
    reconciler.updateContainer(
      createElement(HostContext.Provider, { value: this.host }, node),
      this.container, null, null,
    )
  }
  renderSync(node: ReactNode) {
    reconciler.flushSyncFromReconciler(() => this.render(node))
  }
  /** Sends pending ops; resolves when native acks the transaction. */
  flush(): Promise<void> {
    return this.host.flush()
  }
}

/** An event's update priority, as React DOM assigns it: an update in a
 * press, key, focus or text handler renders synchronously (E19); one in
 * a move, wheel or scroll handler ahead of default work. */
function eventPriority(kind: number): number {
  switch (kind) {
    case EVENT_KIND.pointerDown: case EVENT_KIND.pointerUp:
    case EVENT_KIND.keyDown: case EVENT_KIND.keyUp:
    case EVENT_KIND.focus: case EVENT_KIND.blur:
    case EVENT_KIND.change: case EVENT_KIND.submit: case EVENT_KIND.claim:
      return DiscreteEventPriority
    case EVENT_KIND.pointerMove: case EVENT_KIND.pointerEnter: case EVENT_KIND.pointerLeave:
    case EVENT_KIND.wheel: case EVENT_KIND.scroll:
      return ContinuousEventPriority
    default:
      return DefaultEventPriority
  }
}

export function createRoot(transport: Transport): Root {
  return new Root(new CraieHost(transport, (kind, dispatch) => {
    const outer = priority
    priority = eventPriority(kind)
    try {
      dispatch()
    } finally {
      priority = outer
    }
    // Native delivers events in batches, where the DOM gives each its
    // own task: render a discrete event's updates before the next event,
    // so a second press in the batch sees the first one's state.
    if (eventPriority(kind) === DiscreteEventPriority) reconciler.flushSyncWork()
  }))
}
