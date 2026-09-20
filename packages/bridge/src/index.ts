// React entry point: reconciler config + connect() transport.
//
//   import { createRoot, connect, View, Text } from "@craie/bridge"
//   const root = createRoot(await connect(9470))
//   root.render(<View ...>...</View>)

import React, { createContext, createElement, type ReactNode } from "react"
import ReactReconciler from "react-reconciler"
import { ConcurrentRoot, DefaultEventPriority } from "react-reconciler/constants.js"
import { CraieHost, type HostNode, type Transport } from "./host.js"
import type { StyleProps } from "./wire.js"

export { connect, TcpTransport } from "./client.js"
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
  return createElement("text", props)
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
  removeChild: (parent: HostNode, child: HostNode) => parent.root.remove(child),
  removeChildFromContainer: (root: CraieHost, child: HostNode) => root.remove(child),

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
  // String children become the node's text; no nested text instance.
  shouldSetTextContent: (_t: string, props: any) =>
    typeof props.children === "string" || typeof props.children === "number",
  finalizeInitialChildren: () => false,
  prepareForCommit: () => null,
  resetAfterCommit: noop,
  preparePortalMount: noop,
  detachDeletedInstance: noop,
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
