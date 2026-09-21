// React entry point: reconciler config + native in-process transport.
//
//   host.ts:   runApp(bindings, new URL("./app.tsx", import.meta.url))
//   app.tsx:   const root = attachApp(bindings); root.render(<View ...>...</View>)

import React, { createContext, createElement, type ReactNode } from "react"
import ReactReconciler from "react-reconciler"
import { ConcurrentRoot, DefaultEventPriority } from "react-reconciler/constants.js"
import { CraieHost, type HostNode, type Transport } from "./host.js"
import type { StyleProps } from "./wire.js"

export { attachApp, loadBindings, runApp, NativeTransport } from "./native.js"
export type { Bindings, NativeClientHandle, NativeHostHandle } from "./native.js"
export { Encoder, NIL, type StyleProps } from "./wire.js"
export type { Transport } from "./host.js"

export interface ViewProps {
  style?: StyleProps
  backgroundColor?: string | number
  hidden?: boolean
  children?: ReactNode
}
export interface TextProps {
  style?: StyleProps
  fontSize?: number
  color?: string | number
  hidden?: boolean
  children?: ReactNode // strings land on the wire as text
  text?: string
}

export function View(props: ViewProps) {
  return createElement("view", props)
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
    return createElement("text", { ...props, text: flattenText(children), children: undefined })
  }
  return createElement("text", props)
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
    root.node("text", { text }),

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

  hideInstance: (n: HostNode) => n.root.setHidden(n, true),
  hideTextInstance: (n: HostNode) => n.root.setHidden(n, true),
  unhideInstance: (n: HostNode) => n.root.setHidden(n, false),
  unhideTextInstance: (n: HostNode) => n.root.setHidden(n, false),

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
