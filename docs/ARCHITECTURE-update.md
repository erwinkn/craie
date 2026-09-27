# Architecture update: Marbre UI on Craie

Status: proposal, reviewed 2026-09-24 and 2026-09-25. The decisions below
were settled in review; each moves into `ARCHITECTURE.md` when its work item
lands, and nothing here is current until then. Each topic names the
sections it changes, then gives what the code does today where it matters,
the target, the decisions, the experiments, and what stays open. The last
sections collect the changed decisions, the kit changes this depends on,
the work items and the open points.

## Why

Marbre's UI library, Marbre UI (`@marbre/ui`), is Craie's first large
consumer. Its spec is `packages/ui/docs/ui-kit.md` in the Marbre repository;
the draft sections this update relies on (states and conditions, the motion
language, inline content, placement, lists on Craie) are on branch
`bb/kit-craie-decisions`, with the same decisions as this document. The
kit's rule: every concept is a declarative language, and web, React Native
and Craie each implement it, with a stated fidelity class on each: visually
matched, adaptively matched, approximated or platform-owned. Craie is the
desktop renderer, and its target is visually matched everywhere. Craie
implements the declarations, not the web's or React Native's mechanisms.

The facade below means the bridge's React host (`packages/bridge`): the JS
side that turns React commits into CRW2 transactions and native events into
React calls.

## The rule: native never blocks on JS

React runs on a worker thread, and native never waits for it inside a
frame. That rule stays; what it covers is now stated precisely:

- **Continuous interactions are native.** Scrolling, dragging, typing and
  IME composition, hover, animation and placement run natively from
  declarations that JS sent earlier. JS hears about them as observations.
- **Discrete actions can be claimed.** A node can declare that it claims a
  discrete event: a key chord, a paste, a copy or cut, a drop, a context
  menu request. On such an event native skips its own default action and
  sends JS a claim event; JS runs the action and sends back whatever it
  decides (commands, a structured value to insert). Native keeps running
  meanwhile, and applies the answer when it arrives, rebased onto anything
  that happened in between (topic 11). There is no timeout: a claimed
  action belongs to JS, and a late answer is never applied twice.
- **A claim matches what was on screen.** Declarations travel in the same
  commit as the UI they belong to, so the declaration native matched is the
  one the user saw. Claim events carry the version of the declaration that
  matched, and the facade runs that version's handler (topic 2).

The earlier wording, "native never waits for JS", read as "JS only
observes". It forbade the paste flow that the kit needs and that the claim
mechanism already allows.

## Summary

| Topic | ARCHITECTURE sections | In short |
| --- | --- | --- |
| 1. Claims | §2, §13, §16 | Discrete events claimable by declaration; claim events carry the declaration version |
| 2. Keys | §2, §13, §16 | Modifiers, repeat and composition in key records; chord matching; keymaps; submit policy |
| 3. Focus, press and activation | §13, §14 | Focus traps with `modal`, focus groups, `inert`, innermost press, one native activate, focus-visible |
| 4. Lookup cost | §13, `EXPERIMENTS.md` | Index or walk for each interaction lookup; E15 decides |
| 5. State styles | §3, §12, §13 | Scopes and bits, variant tables with layout values, specificity, inherited color |
| 6. Stacking and geometry | §3, §4, §8, §13, §16 | Sibling z, layers in open order with owners, anchor expressions, sticky |
| 7. Motion | §3, §12 | Keyframe op, enter and exit, scroll timelines, transform parts |
| 8. Paint and text styling | §3, §5, §8, §11, §15 | Shadows, borders, radii, paint sources, layer effects, rings, cursor, text styling |
| 9. Shader nodes | §8, §11, §15 | Fragment programs with parameters and clocks; GPU budget; device-loss recovery |
| 10. Vectors and images | §8, §9, §10, §15 | Runtime path data parsed natively, dashes; images fetched by JS, decoded natively |
| 11. Inline content and editing | §4, §5, §13, non-goals | Inline boxes, decorated and interactive spans, atoms, rebased writes, input features |
| 12. Lists, tables and scrolling | §7, §16, §18 | Templates, change detection, focus-first anchor, tables as two-axis lists, scrollbars |
| 13. Accessibility | §14 | The kit's roles, states and relations; activation without hit testing |
| 14. Environment and platform | §15, §16 | Window state to JS, fonts, clipboard formats, drag regions, literal theme values |

## 1. Claims

Changes: §2 (wire), §13, §16 (event records, protocol version 4).

**Target.**

- A node's interaction row gains a claim set: the discrete event kinds it
  claims (paste, copy, cut, drop, context menu) and its keymap (topic 2).
  The set travels in the interaction op and changes with state: the facade
  sends only the claims whose `when` is true.
- On a claimable event, native looks for a claim on the focused node, then
  on each ancestor up to the root, then (for keys) in the window keymap.
  The first match wins: native skips its default and emits one `CLAIM`
  event: node, generation, the claim kind and index, and the claim set's
  version. An unclaimed event runs its default, then goes to JS as an
  observation.
- A paste or drop claim carries its payload: the plain text and, when
  present, Marbre's in-app rich format (topic 11), or the dropped file
  references. The answer is an ordinary command in a later transaction,
  such as `replace(range, value)` on an input, with the range expressed
  against the buffer revision the claim saw.
- The facade keeps each claim set's handlers until native acknowledges a
  newer version, and runs the version the event names. The user acts on
  what was painted, and what was painted is what native matched.

**Decisions.**

- One event kind, `CLAIM`, for every claimed event (closes K1). The claim
  kind and index share the `key` field; the claim set version travels in
  `revision`.
- Continuous input is never claimable. Typing stays native (topic 11)
  because a per-keystroke round trip waits on whatever JS is doing, and in
  Marbre that is often rendering a streaming reply.
- No timeout on claimed actions.

**Experiments.**

- E19: event round trip under load. Native event to JS handler to applied
  command, measured while JS is idle, while a reply streams at 60 tokens a
  second into a long thread, and across garbage-collection pauses. Report
  the distribution. It bounds what claims cost, and it is the evidence for
  or against routing more through JS later. Measured (`EXPERIMENTS.md`, E19):
  on a quiet heap, native dispatch to React's commit takes 0.2 to 0.4 ms
  at p50 and under 5 ms at p99, even while a reply streams; major
  collections set the tail (p99 35 to 51 ms, max 150 ms, with a 150 MB
  churning heap).

**Built (work item 1).** As targeted, with these choices:

- Claim sets travel in their own op (`CLAIMS`, 0x61: version, then 8
  bytes per claim), not in the interaction op. They change with state
  (`when`) far more often than listeners do, and the interaction row
  stays fixed-size. An empty set removes the node's claims.
- The window list (id NIL) holds key claims only. Copy and cut with
  nothing focused are claimed on the text selection's domain and its
  ancestors.
- A paste claim's answer is `InsertText`: it replaces the selection of
  the focused input inside the claimer, as typed (an undo step and a
  change event). No range or revision yet: those come with rebased
  writes (topic 11), and so does the rich format. Copy and cut answer
  with `WriteClipboard` (NIL may send it); cut then inserts "".
- The session keeps native order between acks and event frames, so JS
  never sees a transaction's ack before an event raised before that
  transaction applied. The facade drops an old version's handlers when
  the ack of the transaction that replaced it arrives, which is then
  exact. Frames carrying claims are never dropped from the bounded
  queue.
- The facade (`@craie/bridge`): `keymap`, `onPaste`, `onCopy`, `onCut`
  (return a string to answer; return nothing to do nothing), `onDrop`,
  `onContextMenu`, `useHotkeys`, `useClipboard`. A version is new only
  when the claims change (chords, kinds, flags), not when a handler
  closure does; the handlers of the current version refresh every
  commit.
- Every `useHotkeys` in the tree shares the one window list, the latest
  mounted hook's bindings first, and the first match wins: a dialog
  that binds Escape beats the page under it. A deviation: Marbre gives
  each hook its own window listener, so two hooks binding the same
  chord both fire there. An unknown chord or submit key is logged once
  and left out (the submit key falls back to `enter`) rather than
  thrown, so a typo does not take the app down.
- Drops are claimed on the path under the drop, with the paths joined
  by NUL. winit 0.30 reports no drop position, and no cursor moves
  arrive while another app's drag is over the window, so today a drop
  lands at an unknown position and goes to the focus path (`LEDGER.md`
  DF-12). On macOS, Ctrl+click is a secondary press, so it opens
  context menus as it does in every Mac app.
- Claims on a nested Text (no native node of its own) are not
  supported yet (`LEDGER.md`, DF-10).

## 2. Keys

Changes: §2, §13, §16.

**Current.** A key record carries a key code and the logical character.
`dispatch.rs` never writes the modifiers into it, while pointer records
pack them into `key`. Space, the function keys and Insert map to
`Key::Unknown` with no character (`platform-winit/src/winit.rs:63`), so JS
cannot see Space. The character has Shift and Option applied (Option+I on
macOS gives "ˆ"), and native copy matches the character `c`, so Cmd+C does
nothing on a Cyrillic layout (`dispatch.rs:620`). Winit reports repeats;
the flag is dropped. Native acts first:

- Tab moves focus, then sends the Tab event to the newly focused node.
- Escape blurs a focused input; with no focus left, the event goes to every
  key listener in the window.
- Enter in a multiline input always inserts a newline.

**Target.**

- Key records carry shift, ctrl, alt and meta, a repeat bit and a
  composing bit, and the physical key next to the logical one. Named keys
  grow to Space, F1 to F24, Insert, ContextMenu and the rest of the
  navigation keys. Protocol version 4.
- **Chords** are a key plus exact modifiers, in the kit's grammar
  (`mod+shift+o`, `arrowdown`, `shift+tab`; `mod` is meta on Apple
  platforms and ctrl elsewhere, and the other of the two must be up). A
  chord matches the logical key without modifiers, lower-cased; for Alt
  with a letter or digit, and when the layout produces a non-Latin
  character, it matches the physical key instead. Native editing commands
  (copy, select all, undo) use the same rule.
- **Keymaps** are claim sets (topic 1): a node claims chords while focus is
  inside it; window chords live in one window-level list, which does not
  match while a text input has focus unless the chord allows it.
- A held key repeats its claim unless the claim says `repeat: false`.
- No chord matches while an IME composes.
- **Input policies**, in the input config: the submit key (`enter`,
  `mod+enter` or `none`). Shift+Enter in a multiline input always inserts a
  newline. Escape has no native action in an input; a keymap gives it one.
- Tab moves inside the innermost active focus trap, and a focus group is
  one stop (topic 3).

**Decisions.**

- Escape no longer blurs an input natively.
- Chord identity is the kit's rule plus the non-Latin fallback.
- Repeat on by default, off per claim (closes K2).

**Built (work item 1).** As targeted, except for one deviation (the
first bullet), with these choices:

- A chord's key is the character with Shift and Alt applied, not the
  key without modifiers: it is what the web's `event.key` gives and
  what Marbre's `matchesChord` compares. So `shift+?` is the chord for
  Shift+/ on a US layout, and `shift+1` matches nothing there (Shift+1
  gives "!"). Alt with a letter or digit, and non-Latin letters, match
  the physical key, as targeted.
- A key record's `key` packs the modifiers (bits 0 to 3), repeat (4),
  composing (5), the named key (8 to 15) and the physical key as its
  US-layout character (16 to 23, 0 off the US layout). Its text is the
  logical character with Shift and Alt applied, as the web's
  `event.key` ("?" for Shift+/). The facade's `KeyEvt.code` is the
  web's `event.code` ("KeyC", "Digit1", "Enter").
- Named keys add Space, Insert, ContextMenu and F1 to F24. Claims beat
  every default, Tab traversal included; Shift+Tab is its own chord.
- The submit key rides in the input config's flag byte. The facade
  mirrors Marbre: `onSubmit` with no `submitKey` means `enter`, and no
  `onSubmit` means `none`. The Rust builder's default is `enter` for a
  single line and `none` for multiline. Submit keys match exactly, as
  in Marbre: Shift+Enter submits nothing in a single-line input and
  breaks the line in a multiline one.
- Native editing commands match exact `mod` chords by the same rule:
  Ctrl+Alt+C no longer copies, and `mod+y` redoes (it undid before).
- A key with nothing focused goes to no one: the whole-tree walk for
  key listeners is gone, and window shortcuts are window-list claims.

## 3. Focus, press and activation

Changes: §13, §14.

**Current.** One `focusable` flag. Each Tab press collects every focusable
node in the window in tree order. A press on space that cannot take focus
blurs. Pointer events go to every listener on the path, so a button inside
a pressable row fires both `onPress` handlers. The facade makes `onPress`
from a primary pointer-up only (`packages/bridge/src/index.ts:225`), so the
keyboard never activates a pressable. An accessibility Click synthesizes a
pointer down and up at the node's center (`ui.rs:268`) and reaches whatever
is drawn there. Window focus loss clears hover and pressed without telling
JS (`dispatch.rs:292`). Removing the focused node clears focus without an
event (`executor.rs:545`).

**Target.**

- **Focus traps** (the kit's `FocusTrap`): `active`, `autoFocus`,
  `restoreFocus` and `modal`. While active, Tab stays inside. On activation
  focus moves to the node marked for auto-focus, else the first focusable
  node. On deactivation or removal, focus returns to the node that had it
  when that (id, generation) is still live and focusable. `modal` makes
  every node outside the trap inert except the layers the trap owns
  (topic 6): the page, earlier popovers and toasts all stop responding.
- **Focus groups** (`FocusGroup`), with orientation, loop and
  `selectOnFocus`: one Tab stop, which is the selected member, else the
  last focused, else the first. Members are the group's focusable
  descendants, only those with the item role when the group has a composite
  role (`radio` in a `radiogroup`, `tab` in a `tablist`). Arrows move in
  tree order and skip disabled and inert members; Home and End go to the
  ends; with `selectOnFocus` native also activates the member reached. JS
  keeps highlight mode and typeahead: focus stays in the control and
  claimed arrow keys move the `highlighted` state.
- **`inert`** on any node: no hit testing, no focus and no accessibility,
  for the node and its subtree.
- **Presses go to the innermost pressable.** The facade marks pressable
  nodes. Press events (down, up, cancel) go only to the innermost marked
  node on the path; raw pointer listeners keep the full path. A disabled
  pressable swallows its presses. Window focus loss cancels a press and
  clears hover, with events.
- **One native activate.** A pointer press, Enter on key down or Space on
  key up on a focused pressable, and an accessibility activate action all
  send one `ACTIVATE` event to the pressable itself, with no hit test. A
  claim on the same chord wins.
- **Keep focus on press**: a node flag (the kit's `preventFocusOnPress`)
  under which a press neither moves nor clears focus, so the composer keeps
  its caret and composition when a suggestion row is pressed.
- **Focus-visible** follows the browsers' rules: keyboard navigation turns
  it on, a pointer press turns it off, a text input shows it however it was
  focused, and programmatic focus keeps the last mode.

**Decisions.**

- Focus traps, focus groups and modality are native. The kit's `FocusTrap`
  gains `modal`.
- No `tabIndex`: tab order is tree order inside traps, a group is one stop,
  and `focusable` and `inert` are the only focus flags (the kit made the
  same change on 2026-09-24).
- Closes F1.

**Open.**

- O1. Where focus goes when the focused node is removed. Proposal: inside
  an active trap, to the trap's auto-focus target, else its first focusable
  node; outside traps, nowhere, as browsers do, with a blur event.

## 4. Lookup cost

Changes: §13. The decision "No BVH or R-tree for ordinary UI" reopens,
but only through E15.

**Current.** Several lookups visit more of the tree than the event needs:

- Hit testing visits every child of every node that does not clip until it
  finds a hit (`hit_node`).
- Each Tab press collects every focusable node in the window (`focusables`).
- A key with no focused node walks the whole tree to find key listeners.
- Each event builds its propagation path as a new `Vec`.

Topics 1, 3, 5 and 6 add lookups: claim routing, focus order, the nodes
that restyle when a state bit changes, and the sorted child order.

**Target.** Each lookup uses the structure that measures best. A
straightforward walk is both the baseline and the oracle.

| Lookup | Straightforward walk | Candidate structures |
| --- | --- | --- |
| The node under the pointer | Tree walk in sorted child order, with clip pruning | The same walk pruned by a visual extent per subtree that layout keeps; a grid or BVH over nodes that can be hit |
| The claim that matches an event | Focus path, a small claim table per node, then the window list | An index from chord to nodes, checked against the focus path |
| Who hears a key with no focus | Whole-tree walk | A listener index per event kind, updated by interaction ops |
| Where Tab goes next | Collect focusable nodes on each press | A focus order per trap and group, updated on structure changes |
| The nodes that restyle when a bit changes | None needed: the facade resolves scope references, so native keeps a dependents list per scope (topic 5) | — |

**Experiments.**

- E15: interaction lookups, index versus walk. Trees of 1k, 10k and 100k
  nodes, both deep (depth 40) and wide (5,000 children), plus a scrolled
  list. Keymaps: 0, 50 and 500. Pointer moves across the window at frame
  rate; key presses with and without focus; Tab through 1,000 focusable
  nodes. Measure the time per event, the allocations, and the memory and
  update cost of each index. An index ships only where it beats the walk
  on the realistic cases and costs less to keep up than it saves
  (principle 7). The walk stays as its oracle (principle 6). Measured
  (`EXPERIMENTS.md`, E15): the hit test takes the first candidate, a
  reach box per subtree kept lazily (one stale bit, refreshed after the
  frame's layout and again before dispatch), 12x to 236x faster than
  the walk (5 to 50 µs a pointer move at 100k nodes, from 0.7 to 1.3
  ms), with upkeep at 2 to 7 percent of the layout pass that caused it
  and 16 bytes per node. Walking a propagation path no longer
  allocates. Tab stays a walk (under 0.7 ms
  at 100k nodes, once per keypress); the key walk with no focus goes
  away with claims' window list (item 1), so neither gets an index.

## 5. State styles

Changes: §3 (new stores), §12 (variants feed transitions), §13 (state
bits). This is probably a new subsection of §3.

**Target.**

- **Scopes.** A node can be a scope, and a scope holds 64 state bits: the
  kit's 10 built-in states and up to 54 custom ones. Native sets the input
  bits: hover, pressed, focus-visible, focus-within and
  focus-visible-within. An op sets the app bits: selected, expanded,
  checked, highlighted, disabled, and custom states. The facade numbers
  custom states in the order the app declares them at runtime (the kit's
  `defineStates`), core states first, then each plugin's in priority order.
- **Scope references are resolved by the facade.** A variant names the
  scopes it reads by node id and generation. The facade resolves "nearest"
  and named scopes (`_row`) through the React tree, so content portaled into
  a layer keeps its owner's scope. Native keeps a list of dependents per
  scope, so a bit change restyles exactly those nodes.
- **Variant tables.** A node with state styles carries its base values and
  its variants. A condition is a conjunction of terms: a scope reference
  with a mask of bits that must be set, and environment bits. A variant's
  values can be any style value: paint, text color, opacity, transform
  parts, and layout values as a partial layout style (the wire's presence
  mask and fields). A layout value is written through `set_layout`, the
  animation driver's writer, and costs one layout pass, with no JS.
- **Environment bits**: narrow, compact, touch and reduced motion, from the
  window size, breakpoints that the facade sets, the pointer kind and the
  platform setting.
- **Order is specificity, and native owns it.** A variant with more
  conditions wins; state and environment terms count, a scope reference
  does not. Between variants of the same depth, the one whose latest-ranked
  condition ranks later wins, with the ranks: custom states in declaration
  order, then hover, focus-within, focus-visible, focus-visible-within,
  expanded, selected, checked, highlighted, pressed, disabled, then narrow,
  compact, touch, reduced motion. Between identical conditions, the later
  declaration wins. Resolution is property by property. Hover and pressed
  do not apply while the scope is disabled, and hover applies only when the
  pointer can hover.
- **Inherited color.** Text spans and vector paints can take their color
  from the nearest ancestor that sets one (`color` inherits, and shapes
  accept `currentColor`). A variant or a tween that changes an ancestor's
  color re-resolves its inheriting descendants every frame, so a row's
  hover color reaches a title with no variant of its own.
- **Interactive spans** (topic 11) are scopes too: a span that is pressable
  or a link has state bits and paint-only variants.
- When a bit changes, native resolves the dependents again, writes the
  changed values through the row writers, and the transition driver tweens
  them as it does for a mutation.
- **Hover follows geometry at rest.** Native recomputes what is under a
  still pointer after frames that moved geometry, but holds hover changes
  during an active scroll and applies them at the settle signal
  (`SETTLE_SECS`), so a wheel scroll does not restyle rows every frame and
  the right row is hovered once scrolling stops.
- One store per fact: for a node with variants, the variant table is the
  source of truth and the row is the resolved input, as for a running
  animation.

**Decisions.**

- State styles resolve natively, layout values included, so a hover or a
  breakpoint never needs JS.
- The facade resolves scope references; native never walks ancestors to
  find a scope.
- Specificity replaces the kit's earlier "a nested key applies right after
  the key that holds it"; the kit spec draft now says the same (closes S2,
  with the 64-bit budget).
- Closes S1: spans get variants through interactive spans (topic 11).

**Built (work item 5).** As targeted, with these choices:

```tsx
defineStates(["unread", "streaming"])
<Pressable group="row" selected={selected} states={{ unread: thread.unread }}
  backgroundColor="#1b1d22" style={{ height: 36, transition: { backgroundColor: { duration: 0.12 } } }}
  _hover={{ backgroundColor: "#24272e" }}
  _selected={{ backgroundColor: "#2d3240", _hover: { backgroundColor: "#343a4a" } }}
  _narrow={{ style: { height: 44 } }}>
  <Text color="#9aa0aa" _unread={{ color: "#ffffff" }}>{thread.title}</Text>
  <View style={{ opacity: 0 }} _row={{ _hover: { style: { opacity: 1 } } }} />
</Pressable>
```

- Four ops in family 0xB0, still protocol 4: `STATES` (a scope's app
  bits), `VARIANTS` (a node's table; empty removes it and restores the
  base), `ENVIRONMENT` (breakpoints, default 1,023 and 639 pt as the
  kit's) and `COLOR` (the inherited color). A node becomes a scope with
  its first `STATES`. Bit index is rank: custom states 0 to 53, then
  hover 54 up to disabled 63; the environment's ranks follow (narrow,
  compact, touch, reduced motion).
- Specificity: depth is the number of state and environment bits a
  variant tests, then the latest rank it tests (its highest bit), then
  declaration order. In the example, hovering a selected row gives
  `#343a4a` (depth 2), a selected one `#2d3240`, a hovered one
  `#24272e`. `_row._focusVisible` beats `_row._hover` (rank 56 over
  54), while `_narrow._hover` against `_selected._hover` compares
  selected with narrow only, both having hover. Variants overlay
  property by property.
- A restyle declares only the fields that differ from the last
  resolved values, so hovering the row above touches its fill and
  nothing else, and a transition tweens exactly that. A table's first
  resolution and the first frame's environment write directly: a row
  mounted selected, or a narrow window at launch, shows no transition.
- Input bits are recomputed from native state after each dispatch,
  transaction and accessibility action (the hovered node's ancestors,
  the primary press's, the focused node's), not tracked per event, and
  skipped when none of those, the modality or the tree changed. Focus
  is visible after a key other than a bare modifier or a Cmd, Ctrl or
  Alt chord, or in a text input. Disabled masks hover, pressed and
  focus-visible. A touch change resolves every table (it masks hover
  everywhere); a width change only the tables that test the
  environment.
- The pointer: leaving the window clears it (hover ends), and a wheel
  event sets it. Hover at rest hit-tests again only when a table or a
  listener reads hover. Detaching a node ends a press inside it; focus
  stays on a node that moves, and its scopes' bits follow it.
- A scope that dies makes its terms false; its id, reused, is a new
  scope (terms hold the generation).
- The node's own ops on a tabled node set its base, and the table
  resolves again. `animate` on a tabled node sets the base too, so a
  variant that overrides the property wins at the next restyle
  (`LEDGER.md` DF-28).
- Inherited color: the nearest `COLOR` on the node or an ancestor, else
  the span's own color. A span inherits unless its Text sets `color`,
  and a Text's own `color` travels as `COLOR` (its spans send white as
  the fallback), so a new color or a tween repaints spans without a
  paragraph op, a shape or a layout. An input keeps its config color,
  and vector `currentColor` reads the Vector's or a `G`'s `color` prop,
  not `COLOR` (DF-24). Inherited color follows the native tree, while
  scopes follow React's: a Text in a `Portal` under a colored Pressable
  keeps the Pressable's scope but draws its own color (white).
- The facade (`@craie/bridge`, re-exported by `@craie/react`):
  `defineStates`; `Pressable` is always a scope, a View with `group`
  (a name makes `_name` address it) is one, and both take `selected`,
  `expanded`, `checked`, `highlighted`, `disabled` and `states`. A
  disabled Pressable stops `onPress`, leaves the Tab order and reads
  as disabled to assistive technology. A `_` key is a state of the
  nearest scope, an environment key, or a group up the tree; other `_`
  keys and every value key a variant does not apply (`pointerEvents`,
  `zIndex`, ...) are logged once and left out. Scopes flow through
  React context, so a `Portal` (new: its children are window roots)
  keeps its owner's; the context Provider is always there, so toggling
  `group` keeps the children mounted. Variant tables resolve to ids
  and go out at the seal, when their signature changed.
- `_hover` on an element that is no scope means the nearest scope's
  hover, where Marbre web means the element's own (`<Text _hover>`);
  inside a Pressable the two agree (DF-29).
- Layout values apply per key: one per property, axis and side. With
  `padding` 16/12, `_narrow: { padding: { left: 4, right: 4 } }` and
  `_compact: { padding: { top: 6, bottom: 6 } }`, a compact window gets
  4/6, and `_narrow: { height: 44 }` keeps whatever width applies
  (another variant's or the base). Suspense's `display: none` wins over
  a variant's `display`. Border color and width are separate values.
- Not yet: transform parts (a variant's transform replaces the whole
  matrix, DF-21), transitions inside a variant (DF-22), text metrics in
  variants (DF-23), a platform source for touch and reduced motion
  (DF-25; `Ui::set_touch` and `set_reduced_motion` exist), variants on a
  nested Text (interactive spans, DF-26), z, pointer events, visibility
  and percent translate in variants (DF-29). Hover can oscillate when a
  hover variant moves the node from under the pointer (DF-27).
- Cost (`states_restyle`, CPU only, exe1 at load about 10, medians of
  three; `EXPERIMENTS.md`, "State styles"): a hover change with 1, 100
  or 1,000 dependents is 0.27, 11 or 116 µs from pointer move to
  patched paint, against 4.2 µs over 1,000 cells with no table; no
  allocation, no layout. Crossing the narrow breakpoint with 1,000
  `_narrow` rows is 0.64 ms, against 0.50 ms for sending the 1,000
  heights directly and 0.30 ms for the resize alone. So a restyle adds
  about 0.1 µs per dependent, and a breakpoint is still one layout
  pass.

## 6. Stacking and geometry

Changes: §3 and §8 (draw order, layers), §4 (a geometry pass), §13 (hit
order), §16 (reconciler portals).

**Target.**

- **Sibling z.** A node's `z` (the kit's named tokens, resolved to numbers)
  orders it among its siblings, as React Native's `zIndex` does. There are
  no stacking contexts. One sorted child order, cached per parent and
  invalidated by structure or z changes, drives both painting and hit
  testing (children tested in reverse of it). Tab order and accessibility
  keep tree order. A z change costs no layout.
- **Layers are React portals** into layer containers under the root, one
  per open overlay, added in open order. Containers sort by their z (the
  kit's `layer` prop: dropdown, overlay, modal, toast, tooltip), then by
  open order. Each container carries its **owner**: the node the facade
  finds through the React tree (a menu opened from a dialog is owned by
  the dialog). An owned layer never sorts below its owner (its effective z
  is at least the owner's), and it belongs to the owner's focus trap,
  `modal` exemption, focus-within and outside-press scope. Containers are
  `box-none` for hit testing. React context passes through a portal, so
  nothing is copied by hand.
- **Geometry expressions** follow CSS Anchor Positioning. A style value
  can be an expression over other geometry:
  - references: a node, a point (a context menu at the pointer), or a text
    range of a text node or input (a menu under the caret);
  - values: `anchor(edge)`, `anchor-size(axis)`, the space available in the
    window or the nearest scroller, and arithmetic with `min`, `max` and
    `clamp`;
  - fallbacks: an ordered list of alternative placements tried when the
    first overflows its boundary (`flip-block`, `flip-inline`, as CSS
    `position-try-fallbacks`);
  - outputs: position, and size limits (width, max width and height).
    Which fallback won and whether the reference is hidden become state
    bits on the node's scope (`_placedTop`, `_anchorHidden`), which variants
    style. The kit writes Floating UI's behaviors (hide when the anchor is
    hidden, an arrow pointing at the anchor) as recipes over these.
- Expressions are style values, as `anchor()` is in CSS: a variant can swap
  one for another, a transition does not apply to them, and a property
  never has two writers. A size expression may not read the node's own
  size or anything derived from it; validation rejects a cycle when the
  declaration arrives.
- **The geometry pass** runs every frame after layout and scroll: it
  evaluates the expressions whose inputs changed, relayouts the subtrees
  whose size limits changed (one small layout each), then sets positions.
  Nothing placed trails a scroll.
- **Sticky** is an expression: `top: max(0, scroller.top − natural top)`.
  The kit brings `position: 'sticky'` back as a portable value for list
  section headers and date separators.

**Decisions.**

- Sibling z replaces "Draw order within a layer is tree order. There is no
  z-index." It matches §8's own target ("draw order derived from tree order
  + z").
- Layers stack by z then open order, with owners from the React tree.
- Modality is strict: toasts behind an open modal are inert. Whether toasts
  draw above dialogs is the kit's token choice.
- Placement is native and generic: expressions after CSS Anchor
  Positioning, not a list of scenarios. Closes L1: a hidden anchor is a
  state the kit styles.

**Open.**

- O2. The expression grammar: the exact functions, units and fallback
  forms, and how the web kit maps them to CSS Anchor Positioning or to
  Floating UI where browsers lack it.

**Built (work item 4, first half).** Sibling z and layers as targeted,
with these choices:

- z is an i32 in the spatial row, sent in the spatial op (mask bit 2).
  The facade reads it from `style.zIndex`, as React Native does (and
  as Marbre's native resolver writes the kit's `z`). A z change bumps
  `structure_rev`, which rebuilds the draw order: no layout, and no
  reach refresh (a reach is a union, whatever the order).
- The sorted order is kept only where it differs from tree order
  (`order.rs`). A parent whose children all have z = 0 and no layer
  holds nothing and never sorts. A tree edit under a sorted parent, a
  child arriving with a z or an owner, and a z change queue the
  parent; the queue is re-sorted before each frame's paint and each
  dispatch (a stable sort: one z among 5,000 zeros is about a linear
  pass), and a reader in between sorts on the spot.
- The native part is generic: a layer op (0x22) makes any node a
  layer container with an owner (a node id, or none). A layer's own
  box lets hits through (`box-none`), and it never sorts below the
  sibling that holds its owner, at any level. Its key is its z and its
  tree position, each raised to at least that sibling's, then one step
  above it:

  ```text
  root:  app z 0, dialog z 70, menu z 50 (owned in the dialog), toast z 80
  paint: app, dialog, menu, toast   (unowned, the menu would go under the dialog)
  ```

  Owners are looked up again after any structure change, so an owner
  that moves takes its layers along; a cycle of owners is cut where it
  closes.
- The facade's `Layer` is a portal:

  ```tsx
  <Layer z={70}>
    <Dialog>
      <Layer z={50}><Menu /></Layer>
    </Dialog>
  </Layer>
  ```

  Its container is a full-window view added at the end of the root
  level when its first child commits (open order), after its owner's.
  It closes once it holds neither children nor an open layer it owns
  (a menu keeps its dialog open), and reopens on top.
  The owner is the enclosing `Layer`'s container, found through React
  context; the app's root nodes are placed before the first layer,
  since React commits a portal's children before its ancestors.
- Cost (`EXPERIMENTS.md`, sibling z; exe1, loaded): re-sorting a
  5,000-child parent after one z change takes 13 to 74 µs. The frame
  after it pays the draw-order walk any structure change pays: 6 to 9
  ms at 100k nodes, against 2.5 to 3.7 ms after a transform
  (`LEDGER.md`, DF-18). Hit tests stay allocation-free: 7 to 12 µs at
  100k nodes, with or without z.
- Focus traps, `modal` and `inert` stay with work item 3, which will
  set finer owners (a trap inside the layer) through the same op
  (DF-19). Tab and accessibility reach layers after the app (DF-17).

## 7. Motion

Changes: §12, §3 (spatial store).

**Target.**

- **An animation op** for a node: keyframes and timing. Keyframes are an
  ordered list of frames; each has an offset from 0 to 1, values, and an
  easing for the segment that follows it. Timing is a duration, a delay, an
  easing (a cubic Bézier curve, steps, linear points or a spring), a count
  of iterations or infinite, a direction and a fill. A value that the first
  or the last frame leaves out is the node's resolved value. Frames can hold
  opacity, transform parts, paint colors, a paint source's offset (topic 8),
  a layer blur radius, and the stroke dash offset of vector shapes; exit
  frames can also hold height and width.
- **Timelines.** An animation's progress comes from the clock (default),
  from a scroller's offset over a range, or from a node's progress through
  its scroller's viewport, as CSS scroll-driven animations do. A scroll
  timeline samples in the same frame as the scroll.
- **Triggers:**
  - `enter`: the op arrives in the batch that creates the node and starts
    when the node is created. It runs on every creation; the kit gives it
    only to content that arrived live, because list rows are created again
    when they scroll back into view.
  - `exit`: a `detach` op can carry an exit animation (see below).
  - `animation`: runs while it is part of the node's resolved style. In a
    base style, it starts when its op arrives and stops when an op removes
    it. In a variant (topic 5), native starts and stops it when the variant
    starts or stops applying, with no JS.
  - `transition` stays as it is.
- **Exits hold a subtree, and JS waits to reuse its ids.** React removes a
  subtree with one `detach` of its root, then one `remove` per node, and
  the facade recycles each id at once (`packages/bridge/src/host.ts:679-703`).
  So the exit rides on `detach`, which is the moment the node would leave
  layout. Native keeps the whole subtree in its layout place, drawn, with
  no hit testing, focus or accessibility, until the animation ends; frames
  that tween height or width to 0 let the siblings close the gap through
  ordinary layout. The facade parks the subtree's ids instead of recycling
  them, and frees them on the exit-end event. If the parent goes while an
  exit runs, native frees the subtree at once and reports the exit ended.
  Only the removed root's exit runs; a descendant's exit runs only when
  that descendant is itself the root of a removal.
- **Reduced motion resolves in JS.** The facade applies each animation's
  policy before it sends the op: `skip` sends nothing, and `fade` sends the
  opacity frames only. Native reports the platform setting (topic 14).
- Loops run on the native clock. A running loop keeps frames coming, and
  an occluded window stops them, as §15 already does for tweens.
- **Transform parts.** The spatial store holds translate x and y, rotate,
  scale x and y, and the free matrix as separate values, composed in CSS
  order: translate, rotate, scale, then the matrix. Each part tweens on its
  own, and rotate tweens by angle, so 0 to 360 degrees is a full turn.
  Matrix decomposition stays for the free matrix only. A variant, a
  transition and an animation can each move a different part of one node.
  A percentage translate resolves against the node's own size.
- `animate` stays for one-off tweens to a target.

**Decisions.**

- Keyframes, loops, exits and scroll timelines are native. Today a loop
  would re-issue `animate` from JS after each end event.
- Exits: the facade keeps the subtree's ids until the exit ends (closes
  M1); the node keeps its place and may collapse its size (closes M2).
- Transforms are stored as parts. For the parts, this replaces
  "Transforms interpolate as rotation x upper-triangular x translation".
- "First values at mount do not tween" stays for transitions. `enter` is
  how a node animates at mount.
- M3 is settled in the kit spec: a retargeted spring keeps its velocity on
  Craie and loses it on web and React Native, which are approximated.

## 8. Paint and text styling

Changes: §3 (paint store), §5, §8, §11, §15 (cursor in the platform
contract).

**Target.**

- **Box paint**: a shadow list, a border width for each side (solid or
  dashed), a radius for each corner, and a fill from a paint source. A
  shadow layer has x, y, blur, spread, color and inset, and layers draw in
  order; the kit already sends them structured (`elevation.card` is a ring
  layer plus a shadow stack).
- **Rings** are outlines: the kit's `ring` and `ringOffset` draw a 2 point
  stroke around the border box at an offset, a rounded rectangle that
  follows the corner radii. Elevation rings stay shadow layers.
- **Paint sources.** Any rect, glyph or mesh takes its color from a paint:
  a solid color, or a linear or radial gradient from the paint pool that
  step 5a added for meshes. A gradient's offset is animatable. Shimmer text
  is a gradient on the glyphs of a span with a moving offset; box gradients
  (`bgGradient`) use the same records.
- **Layer effects.** The isolated layer that opacity uses today gains an
  effect list: opacity, then blur with an animatable radius (a separable
  Gaussian, downsampled for large radii). `blur-in` animates the radius.
  The same pass is a candidate for shadow rendering (P1) and later for
  backdrop blur behind overlays.
- **Hit-test policy** per node: normal, `none` (the node and its subtree
  let the pointer through) or `box-none` (the node lets the pointer
  through, its children do not).
- **Visibility**: `hidden` keeps the node's layout, draws nothing, and
  makes the subtree inert.
- **Cursor** per node, read from the hovered path and set through the
  platform contract.
- **Text**: a line clamp with an ellipsis, text alignment, OpenType
  features per span (tabular digits first, then the theme's `font.features`),
  no-wrap, decoration color, and text transform (uppercase, capitalize).
  Balanced and pretty wrapping come later.

**Decisions.**

- Shadows, easings and fonts arrive structured, so Craie never parses CSS.
  The kit made these tokens structured on 2026-09-24.
- Shimmer and blur are native (visually matched), through two general
  primitives (paint sources and layer effects) rather than one shader per
  effect.

**Experiments.**

- P1: shadow rendering, an analytic blur of a rounded rectangle against a
  blurred mask through the layer blur. Includes inset shadows with a
  different radius per corner.
- E20: layer blur cost. Twenty staggered `blur-in` entrances in one frame
  and a full-window backdrop blur, at 1x and 2x; GPU time and offscreen
  memory per blurred layer.

## 9. Shader nodes

Changes: §8 (a new chunk kind), §11 (runtime pipelines), §15 (device loss).

**Target.**

- A shader node draws its content box with a fragment program: the
  generative orbs of voice and agent interfaces (the orbkit style), and
  similar backgrounds. The program reads the pixel position, the node's
  size, a clock, and named numeric and color parameters. The node is laid
  out, clipped, transformed, hit-tested and made accessible like any node,
  and draws as one quad in painter order.
- Programs arrive as WGSL at runtime, compile once per source through
  wgpu, and are cached. A compile error is reported to JS and the node
  draws its fallback: a still image or a gradient that the kit supplies.
- Parameters animate like any animatable value. A parameter marked
  `integrate` is a clock of its own (`phase += dt × rate`), so a change of
  rate changes speed without jumping the phase.
- A shader node requests frames while it is visible and its window is not
  occluded, like a loop; the frame costs only its parameter upload on the
  CPU side.
- **Plugins may ship shaders.** Native times each shader node on the GPU
  with timestamp queries; a node over its per-frame budget is replaced by
  its fallback and reported to the plugin diagnostics.
- **Device-loss recovery.** A shader that hangs the GPU makes the OS reset
  the device. Today Craie reconfigures a lost surface (`app.rs:465`) but
  cannot survive a lost device. Target: rebuild the device and pipelines,
  and upload everything again from the CPU side, which already exists (the
  atlas keeps page mirrors; buffers upload from retained staging).

**Decisions.**

- Shader nodes are a primitive, not a style on any node. The kit's
  `Shader` has one source per platform (GLSL on web, SkSL on React Native,
  WGSL on Craie), as `Chart` has one renderer per platform.

**Open.**

- O3. The shader source strategy: three hand-written sources, or one
  common subset translated per platform (naga reads Vulkan-style GLSL, not
  the WebGL 1 dialect that orbkit uses).
- O4. The budget's value and how it scales with the node's pixel area.

## 10. Vectors and images

Changes: §8, §9 (runtime path data), §10 (image residency), §15 (decoding).

**Current.** Vectors come only from build-time CRV1 assets (usvg), and
`craie-vector` has no dashes. Images are not implemented (§8 target lists
`ImageInstance[]`; §15 decides that images decode in the platform adapter
and that the core owns `ImageId`, dimensions, format and residency).

**Target.**

- **Runtime vector shapes.** A Vector node's shapes arrive as SVG strings:
  path data (`d`), `points`, the `transform` attribute, the `viewBox`, and
  dash arrays. A small native parser turns them into paths (not usvg: no
  documents, styles or text). Tessellation is cached per shape and display
  scale. Fills and strokes take paint sources and `currentColor`
  (topic 5). Strokes gain dashes, with an animatable dash offset (spinner
  rings). This serves the kit's runtime icon registry, plugin icons, brand
  artwork and data-driven shapes (sparklines, meters, progress rings).
- **Images.** The facade fetches the bytes (HTTP and caching stay in JS
  and the app) and sends them once as a payload. The platform adapter
  decodes them off the UI thread and downscales to the displayed size at
  the display scale (a 4,000 pixel photo shown at 40 points takes 80 pixels
  of texture at 2x, not 64 MB). The core owns `ImageId` and residency, as
  §15 decided. The node supports `fit` (cover, contain, fill) and reports
  load failures to JS.

**Decisions.**

- Native parses path data at runtime. The §9 decision "SVG imports at build
  time only" now reads: SVG documents import at build time only; path data
  is runtime input.
- JS fetches images and native decodes them; Rust gets no HTTP client.

**Built (work item 8, runtime vector shapes).** As targeted, with these
choices:

- One op, `DRAWING` (0x72), replaces a Vector node's whole drawing: a
  view box and up to 4,096 shapes of 44 bytes (kind, fill rule, join,
  cap, three string refs, fill and stroke colors, width, miter limit,
  dash offset, opacity). The strings stay SVG syntax and native parses
  them (`craie_vector::svg`, about 600 lines, no dependency). A drawing
  whose strings or numbers do not parse draws nothing (zero intrinsic
  size) and the session goes on, as SVG draws nothing for an empty view
  box; structural errors (bad refs, too many shapes, too many bytes)
  reject the transaction.
- Work is bounded per drawing, not per string: at most 4,096 shapes
  whose string references add up to at most 4 MiB (a string shared by
  every shape counts for each), checked before anything else; each
  distinct string parses once; path commands (per shape drawing them),
  transform functions and points share one budget of 2^20, and dashes
  one of 65,536 per drawing.
- A drawing builds the same `Asset` a `CRV1` payload decodes to, so
  layout, fitting, clipping and tessellation are shared. Sources are
  interned by content (payload bytes, which must start `CRV1`, or a
  drawing's canonical key, which starts `CRVS`, built once per
  transaction and shared) and meshes are shared across nodes per asset,
  content box and scale: the cache is per drawing, not per shape.
  Tessellation skips shapes outside the view box or at opacity 0, and
  flattens no finer than a shape's size over 2^16.
- Dashes restart on each subpath, as SVG does; zero-length dashes draw
  as dots with round or square caps; a closed subpath joins its last
  dash to its first. Past the drawing's dash budget, a pattern draws
  solid rather than stall.
- The dash offset is a plain value, not animatable: `ANIMATE` (0xA1)
  animates node properties, and an offset belongs to a shape, so a
  spinner ring rotates the node instead (`LEDGER.md` DF-13).
- Paints are plain colors. `currentColor` resolves in the facade to a
  `color` prop on the `Vector` or a `G`, as `<svg color>` does, and
  throws without one; inheriting a color from ancestors waits for
  topic 5 (DF-14). `fillOpacity` and `strokeOpacity` scale the alpha.
  Gradients stay build-time.
- The facade flattens children into shapes: `Path`, `Circle`,
  `Ellipse`, `Rect`, `Line`, `Polyline`, `Polygon` and `G` (attributes
  inherited, transforms nested, a `G`'s opacity multiplied into its
  shapes: DF-16), with the Vector's own paint props as defaults, as on
  an `<svg>` element. The Vector's `opacity` is the node's: one layer,
  animatable. Numbers are coerced as SVG reads attributes; a shape with
  one that is not finite is dropped, with a warning. The kit's shapes
  differ in three ways: token colors must be resolved first, its
  `Path` and `Circle` wrappers use hooks and so throw (shapes must be
  direct elements, DF-15), and inherited color needs `color` (DF-14).
- Cost (exe1, loaded; `cargo run --release -p craie-harness --example
  vectors`, the Rust direct API: no wire, no JS): 200 distinct 24 px
  icons parse in about 0.4 ms and mount in about 2 ms more than 200
  plain views (10 µs per icon, tessellation included); an icon already
  drawn elsewhere costs about 3 µs more than a plain view; a
  2,000-point sparkline parses in 90 µs. In JS, an unchanged icon
  costs about 5 µs a render (flatten, stringify, compare).

**Built (work item 8, images).** As targeted, with these choices:

```tsx
<Image src="https://example.com/photo.jpg" fit="cover" alt="Avatar"
  style={{ width: 40, height: 40 }}
  onLoad={e => console.log(e.width, e.height)}   // natural pixels
  onError={e => console.warn(e.message)} />
```

- An Image node is kind 7. Its encoded bytes are a `PAYLOAD` (0x71);
  `IMAGE_CONFIG` (0x73) sets the fit (default cover). Native accepts any
  bytes: a bad image fails later as an event, not a rejected
  transaction. Out-event 18 reports load (key 0, the natural size) and
  failure (key 1, the reason); both are reliable.
- The core owns each payload's `ImageId` and plans the decode: the
  source rect (cover crops the centered part with the box's aspect) and
  the pixel size (the drawn size at the display scale, never above the
  source, at most a page). A probe first reads the header for the
  natural size, which is the intrinsic size (a pixel per point, like a
  Vector's view box). A box that grows asks for 1.25 times the size it
  needs, so a steady resize redecodes every 25 %; one that shrinks keeps
  its bitmap down to half the size, and a cover crop within 2 device px
  keeps it too (DF-34, DF-37). A new `src` keeps the old image and its
  natural size until the new pixels or failure (DF-38).
- The platform decodes on one worker thread (`image` crate: PNG, JPEG,
  WebP, GIF's first frame; EXIF orientation applied) and averages the
  crop down, premultiplied. It rejects images over 64 megapixels at the
  header and reserves its buffers against 512 MiB before decoding (a
  250 KB PNG can claim 16,000 x 16,000). Probes jump the queue, queued
  work for dropped images goes, and a codec panic fails only its image.
  The core keeps the pixels (64 MB for all images, then decode again)
  and puts them in the color atlas as a raster; the quad is a color
  glyph, so images need no new instance type or shader. The gutter
  repeats a color raster's edge pixels, so magnified edges keep their
  color. No color management yet (DF-35).
- The facade fetches URLs (http, https, data, blob, file, paths) once
  per `src`, or takes a `Uint8Array`, and keeps the old image until the
  new one arrives. A failed or timed-out (30 s) fetch clears the image
  and fires `onError`; unmounting aborts the fetch. `alt=""` is
  decorative: no role, no label. Kit gaps (numeric `src`, SVG data URLs,
  ICO, placeholder and fallback, own-radius clipping) are DF-31 and
  DF-32; images are not shared across nodes (DF-30).
- Cost (exe1, loaded; `cargo run --release -p craie-platform-winit
  --example images`): a 4,000 x 3,000 JPEG (1.9 MB) probes in 0.06 ms
  and decodes to an 80 x 80 cover in 60 to 80 ms (most of it the full
  decode), for 25.6 KB of texture instead of 48 MB; the same photo as
  PNG takes about 120 ms, as 16-bit PNG about 220 ms. One worker
  serializes decodes (DF-33).

## 11. Inline content and editing

Changes: §4 (the decision "Block means block-level boxes only. Inline
content is always a Text paragraph node."), §5, §13 (editing), non-goals
("Inline formatting context", "controlled inputs").

**Current.** An input's native config holds a font size, a color, a
placeholder and the multiline flag; it cannot take the theme's font
family, weight or line height. Inputs are uncontrolled: `value` is sent once
at mount and `setText` replaces the text.

**Target.**

- **Inline boxes.** A Text node can have element children. Each child
  becomes an inline box at its position in the text, with the rules of CSS
  `inline-block`:
  1. Layout measures the box against the paragraph's full line width, not
     the space left on the line, so the box's size does not depend on where
     it lands. The paragraph measures its boxes once for each width that
     layout tries, and caches them.
  2. The paragraph holds one object replacement character for the box,
     with the box's width as its advance. Line breaking treats it as one
     cluster that never splits, with break opportunities on both sides.
  3. A line grows to fit its tallest box.
  4. `verticalAlign` is baseline (the box's first baseline, from the
     layout results), center, top or bottom.
  5. After line breaking, the paragraph positions the box's node. The box
     paints, hit tests and scrolls as an ordinary node.
  6. Selection and copy treat the box as one cluster, whose text is the
     box's `textValue`. Accessibility reads it in text order.
  Element children inside nested Text become children of the paragraph's
  native node, at their position in the flattened text.
- **Decorated spans.** A span can carry a background, a radius, a border
  and horizontal padding. Each line that the span covers gets its own
  rectangle, from the range geometry that selection already uses
  (`Paragraph::selection_rects`). Padding and end radii apply only at the
  span's two ends, as with CSS `box-decoration-break: slice`.
- **Interactive spans.** A span can be pressable, focusable and a link,
  with a scope's state bits and paint-only variants (color, underline and
  its color, background). Native already finds the span under the pointer;
  focus can rest on a span, its focus ring draws from its per-line
  rectangles, and accessibility exposes it as a link over its text range. A
  markdown link in `RichText` is an interactive span and wraps like the
  words around it; an inline box would jump to the next line whole.
- **Editor atoms.** An input's buffer can hold atoms. Each atom is one
  object replacement character in the buffer plus an entry in an atom
  table, with its key, kind and text. The editing rules:
  - The caret moves over an atom in one step.
  - Backspace or Delete next to an atom first selects it. A second press
    deletes it.
  - Selection, cut, copy and undo treat an atom as one character.
  - Copy writes the atom's text as plain text, and the structured value in
    Marbre's in-app clipboard format next to it.
  - Paste inserts plain text unless the input claims paste (topic 1). The
    claim receives the plain text and the in-app value, and JS answers
    with the value to insert, deciding which atoms to keep (for example,
    after checking a mention against files and plugins).
  - An IME composition never starts inside an atom and never contains one.

  A `replace(range, value)` command inserts atoms, and change events carry
  the structured value. The facade renders each atom as a child node, and
  native draws it as an inline box. An atom whose node is not there yet (it
  came back through undo, and React has not rendered its chip again) keeps
  its last measured size, as a list item keeps its last measured height
  while its row is not rendered.
- **Controlled values through rebased writes.** Native owns the buffer, and
  every native edit advances its revision. When the kit's controlled
  `value` differs from the last value native reported, the facade diffs it
  against the value at the revision it saw, sends the smallest range edit
  with that revision, and native maps the edit past its newer edits, as
  collaborative editors merge concurrent edits. Picking "@Jo Smith" for
  `@jo` after the user typed `h` gives `@Jo Smith h` with the caret after the
  `h`; clearing after send clears what was sent and keeps what was typed
  since. A paste answer uses the same mapping, so text typed while JS
  handled the paste lands after the pasted content.
- **Input features in v1**: the full text style from the theme (family,
  weight, line height, letter spacing), placeholder color, `rows` and
  `maxRows` autosize natively, password fields (masked, no copy, no IME),
  `maxLength`, disabled and read-only. Spell checking comes later.

**Decisions.**

- In the non-goals, "inline formatting context" becomes "inline boxes that
  break across lines, and floats".
- The §4 decision becomes: "Block means block-level boxes only. Inline
  content is a Text paragraph node, which can hold atomic inline boxes and
  interactive spans."
- §5 "Uncontrolled inputs only" becomes: native owns the text; the facade
  presents controlled values as range edits that native rebases (closes
  I1). The non-goal "controlled inputs" goes.

**Experiments.**

- I2: chips that stream in and change size rewrap their paragraph with no
  new shaping. Measure the rewrap cost at 1, 10 and 100 size changes per
  frame in a long paragraph, and set a budget from it.

## 12. Lists, tables and scrolling

Changes: §7, §16, §18.

**Current.** The facade proves that a measured height is still valid with
the `unchanged` flag: the item is the same object it removed under that
key. The React `List` diffs an array. Scroll offsets are `f32`, and a
scroller counts as at its end within 0.5 points (`list.rs:733`). Craie draws
no scrollbars and scrolls only on wheel events.

**Target.**

- **Craie runs its native list.** The kit's JS virtualizer runs on web and
  React Native; the kit spec's Craie column now says so. The two stay
  identical through shared scenarios (below).
- **Estimates are templates.** The kit's list contract replaces
  `estimateSize` with `templates` and `describeItem(index)`, which returns
  `{ template, textLength? }`. A template is a fixed extent, a table of
  extents by column width (as the chat page's `kindEstimate` is), a fixed
  extent plus text wrapped at the list width, or an aspect ratio for media
  (closes V1). Native computes pixels with real font metrics, as §7
  decided; web and React Native compute them from the same data.
- **Changes are found per commit.** The count-based API gives each item
  `loaded` and `version` (the kit's `isLoaded` and `itemVersion`). On each
  commit the facade compares the mounted items' loaded state and version,
  by key, with the last commit, and marks the changed ones in the splice
  it sends; a count change also locates its one contiguous change by the
  keys at both ends. Off-screen items keep their estimates or measurements
  until they mount, and the anchor absorbs the difference.
- **The anchor rule.** Before an update applies, a scroller's anchor moves
  to the visible item that holds focus, else to the topmost visible item
  the update leaves unchanged (loaded ones first), else the topmost visible
  item. A page of history replacing placeholders does not move what the
  reader sees, and a card expanded at the top of the screen grows
  downward, under the pointer that expanded it.
- **Scroll to item and follow.** A command scrolls to an item by key, with
  an alignment of start, center or end; the target becomes an explicit
  anchor, and native recalculates the offset from it as rows get measured,
  until the user scrolls. `followKey` scrolls to the end in the commit that
  carries a new key.
- **The end threshold is per scroller**, from the kit: 80 points for lists
  and 48 for plain scroll views, both for `stick-to-end` and for the at-end
  event.
- **Precision.** Rendered rows get positions relative to the first
  rendered row, calculated in `f64` from the extents and converted to
  `f32`. Each scroller keeps its offset in `f64`. Scroll events carry the
  `f64` offset split across two record fields, so the kit's
  `scrollOffset()` and `scrollToOffset()` stay exact at any length.
- **Restore.** The kit's `restoreKey` maps to the native list's saved
  measurements (closes V2).
- **Tables are lists with a second axis.** One engine: a size index per
  axis, pinned bands on both axes (the header and footer rows, pinned
  leading columns), anchoring and range events. React renders rows as
  nodes tagged with their row index, each holding the cells of the visible
  columns tagged with their column index; native places both, sizes a row
  to its tallest cell, and keeps pinned bands in place while scrolling in
  either direction. Rows are scopes (hover, selected) and accessibility
  rows. Column widths are native during a resize drag: native moves the
  column edge every frame and reports throttled progress and the final
  width, and the kit's controlled widths catch up.
- **Scrollbars and keyboard scrolling are native.** A scroll node draws
  scrollbars per the platform setting (overlay while scrolling, or always
  shown), with a draggable thumb and paging on the track, colored from the
  theme. PageUp, PageDown, Space, the arrows, Home and End scroll the
  focused scroller unless a claim takes them.
- **Shared scenarios.** Marbre moves the scenarios in its
  `virtual/viewport.test.ts` to data files, with steps and expected
  anchors. A Rust runner in the harness checks the native list against the
  same files.

**Experiments.**

- E16: overscan that leans toward the scroll direction, as in Marbre: 1.5
  times ahead and 0.5 times behind. Compare it with symmetric overscan and
  hysteresis. Measure blank rows during fast scrolls, and rows rendered.
- E17: a first render with only the visible rows, and the overscan in the
  next commit, versus the full range at once. Measure the time to the
  first paint of a long thread.
- E18: precision at 50M points, before and after relative positions.
  Measure whether small scroll steps still move the rows, whether
  neighboring rows touch exactly, and whether content after the list in
  the same scroller (laid out in `f32` after a 50M-point list) meets the
  last row exactly.

**Decisions.**

- Native list on Craie, aligned by shared scenarios. This keeps §7's "no
  metrics or extents copy lives in JS".
- Tables are the list with a second axis, with React cells.

**Open.**

- O5. Column windowing: render every column of the rendered rows at first
  (40 rows by 60 columns is 2,400 cells) and measure before windowing the
  column axis.

## 13. Accessibility

Changes: §14. Related: `LEDGER.md` DF-1 (per-cluster text runs).

**Target.** Roles for the kit's full role set. States: checked (and
mixed), selected, expanded, pressed, disabled, busy, invalid, required,
read-only, current, modal and the popup type. Relations by node reference:
labelled by, described by, controls and active descendant (the kit passes
refs; the facade resolves them to nodes). Live regions, heading levels,
orientation, autocomplete, sort, value ranges and value text, and table
row and column indices and counts. Atoms, inline boxes and interactive
spans read in text order. An accessibility activate action goes through
native activation (topic 3), not a synthesized pointer.

## 14. Environment and platform

Changes: §15, §16.

**Target.**

- Layout events, opt-in per node: size and position after layout, for
  charts and resizers.
- Window state, reported to JS when it changes: size, scale, pointer kind
  (can hover or not), reduced motion, color scheme, increased contrast,
  font scale, and window focus. The same values drive the environment bits
  (topic 5).
- Font registration from JS: font bytes into the font store (`RawFonts`
  exists natively). The desktop app ships TTF or OTF files; WOFF2 stays a
  web format.
- Clipboard read and write as commands and results, with plain text plus
  Marbre's in-app rich format (a private pasteboard type on macOS).
- A window drag region: a node flag that lets the custom title bar move the
  window.
- File drops arrive as claimable events (topic 1).
- **Theme values are literal.** The facade resolves tokens to values, and a
  light or dark switch re-renders the themed components and sends their new
  values once. Native keeps no token table.

**Experiments.**

- E21: the cost of a mode switch with literal values on the 10k-node and
  the 50,000-message workloads: JS render, bridge bytes, and native apply.

## Changed decisions and non-goals

| Where | Now | Proposed |
| --- | --- | --- |
| This update's rule | "Native never waits for JS" | "Native never blocks on JS"; discrete events can be claimed |
| §4 Decisions | "Inline content is always a Text paragraph node" | A Text paragraph can hold atomic inline boxes and interactive spans |
| §5 Decisions | "Uncontrolled inputs only" | Native owns the text; controlled values arrive as rebased range edits |
| §7 Current | The `unchanged` flag proves a measurement valid | Per-commit loaded and version changes of mounted items; templates and `describeItem` |
| §8 Decisions | Draw order from tree order | Tree order plus sibling z; layers by z then open order; layer effects; shader nodes |
| §9 Decisions | "SVG imports at build time only" | SVG documents at build time; path data parsed at runtime |
| §12 Current | Transforms interpolate by matrix decomposition | Transform parts, composed in CSS order |
| §12 Decisions | "First values at mount do not tween" | Kept for transitions; `enter` covers mount; scroll timelines join the clock |
| §13 Decisions | "No BVH or R-tree for ordinary UI" | No separate spatial index (R-tree, rebuilt BVH): the node tree carries a bounding box per subtree, which makes the hit test 12x to 236x faster (E15) |
| §13 behavior | Escape blurs inputs; Tab and Enter act before JS; presses reach every listener | Claims, input policies, focus traps and groups, innermost press, one activate |
| §2 Decisions | "The ack remains only to resolve `flush()`" | The ack also retires old claim handlers; the session keeps acks and events in native order |
| §15 Target | No device-loss handling | Rebuild the device and upload everything again |
| Non-goals | "Inline formatting context" | Inline boxes that break across lines, and floats |
| Non-goals | "controlled inputs" | Removed: controlled values are rebased writes |

## Kit changes this depends on

Written into the kit spec draft on `bb/kit-craie-decisions` (decisions
D16 to D27 there):

- Specificity for variant order; `defineStates` for custom state order.
- `KeyClaim.repeat`; claims for paste, copy, cut, drop and context menu.
- `FocusTrap.modal`; Enter, Space and screen-reader activation call
  `onPress`.
- Sibling `z`; layers by z then open order, never below their owner.
- Anchor expressions for placement; `position: 'sticky'` back as a portable
  value.
- Interactive spans for links in prose; rich paste decided by a claim.
- `templates` and `describeItem` replace `estimateSize`; focus-first
  anchor; per-scroller end threshold; the Craie column says native list.
- Exits keep their place and may collapse their size; only the removed
  element's exit runs; `enter` on every mount; scroll timelines.
- A `Shader` primitive.

## Work items

The order follows the dependencies:

1. Claims and keys: key records, chord matching, keymaps, submit policy,
   claim events with versions, paste and drop claims (topics 1, 2).
   Protocol version 4.
2. E15, then the lookup structures it selects (topic 4), before or with
   items 1, 3, 4 and 5, which add lookups.
3. Focus traps with `modal`, focus groups, `inert`, innermost press, one
   activate, keep-focus flag, focus-visible (topic 3). `modal` needs
   `inert` and item 4's owners.
4. Sibling z and the sorted child order; layer containers with owners
   (topic 6, first half).
5. State styles: scopes, variant tables with layout values, specificity,
   inherited color, hover at rest (topic 5). Item 6 needs it, because
   animations live in variants, and item 9 needs it for placement states.
6. Motion: the animation op, exits, transform parts, scroll timelines
   (topic 7).
7. Paint and text styling, paint sources, layer effects (topic 8).
8. Runtime vectors with dashes; images (topic 10).
9. Geometry expressions: placement, sticky (topic 6, second half).
10. Inline boxes, decorated and interactive spans, then editor atoms,
    rebased writes and the v1 input features (topic 11). Atoms need
    inline boxes; rich paste needs item 1.
11. Lists: templates, change detection, the anchor rule, thresholds,
    precision, restore, shared scenarios; then tables; scrollbars and
    keyboard scrolling (topic 12).
12. Shader nodes, the GPU budget and device-loss recovery (topic 9).
13. Accessibility (topic 13).
14. Environment and platform (topic 14).

## Open points

| Id | Topic | Question |
| --- | --- | --- |
| O1 | Focus | Where focus goes when the focused node is removed |
| O2 | Geometry | The expression grammar, and its mapping to CSS Anchor Positioning and Floating UI on web |
| O3 | Shaders | One translated shader source or one per platform |
| O4 | Shaders | The GPU budget's value and scaling |
| O5 | Tables | When to window the column axis |
| P1 | Paint | The shadow rendering technique (experiment) |
| I2 | Inline | The rewrap budget for chips that change size (experiment) |
| — | Kit | The namespace rule for plugin state names; whether toasts draw above dialogs |

Closed in review: K1, K2 (topic 2), F1 (topic 3), S1, S2 (topic 5), M1,
M2, M3 (topic 7), I1 (topic 11), L1 (topic 6), V1, V2 (topic 12).
