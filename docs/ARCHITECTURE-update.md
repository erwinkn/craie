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

- O1, closed (work item 3): as proposed. When the focused node is
  removed or becomes inert, focus goes to the target of the innermost
  active trap that held it (its auto-focus node, else its first
  focusable); outside traps, nowhere, with a blur event.

**Built (work item 3, traps and inert).** Focus traps, `modal`, `inert`
and finer layer owners, as targeted. Focus groups are another PR;
presses, activation, keep-focus and focus-visible are #17's (the next
Built paragraph). The example:

```tsx
<Pressable onPress={() => setOpen(true)}>Delete…</Pressable>
{open && (
  <Layer z={70}>
    <FocusTrap modal>
      <View accessibilityRole="dialog">
        <Pressable onPress={close}>Cancel</Pressable>
        <Pressable autoFocus onPress={remove}>Delete</Pressable>
        <MoreButton />  {/* opens <Layer z={50}><Menu/></Layer> */}
      </View>
    </FocusTrap>
  </Layer>
)}
```

Opening focuses Delete. Tab cycles Cancel, Delete, More, then the menu's
items when it is open. The app stops answering the pointer, Tab and
assistive technology. Closing returns focus to "Delete…".

- **Encoding** (protocol 8). Interaction flag bits 2 (`INERT`) and 3
  (`AUTO_FOCUS`), next to #17's press bits 4 to 6; op 0x62 `TRAP`
  (`id u32 | flags u8`: `ACTIVE`, `MODAL`, `AUTO_FOCUS`,
  `RESTORE_FOCUS`); roles 17 `dialog` and 18 `alertdialog`. 0x63
  stays free. The facade's `FocusTrap` is a View carrying the op, so
  a trap is any node.
- **Settling.** Trap flags take effect at the end of the transaction
  (`trap.rs`, `settle_traps`), after the tree is complete. Traps
  deactivate and activate first. Then focus moves once, so one blur
  and one focus event, to the last of these that applies:
  1. the outermost deactivating trap's restore target;
  2. the transaction's `focus()`;
  3. a new trap's auto-focus target, deepest trap first;
  4. an `autoFocus` node mounting into an active trap the focus is
     outside of (a wizard step loading in);
  5. O1, for a focus that was removed or ends blocked or hidden.

  The modal gate is rebuilt last, on every transaction while a trap
  is declared. A restore needs the saved (id, generation) still live,
  focusable, shown and not inert; a reused id fails the generation
  check. The saved focus is the one when the transaction began, so a
  `focus()` from the dialog's own layout effect, sealed with it, is
  not what closing returns to. A trap opening as another closes
  inherits the closing one's target. Nested traps that activate
  together restore once, through the outer one: closing only the inner
  one keeps focus inside the outer one.
- **`focus()`.** A Focus command the traps allow applies at once (a
  text insert may follow it); one they block waits for the settle and
  is judged against the traps the transaction leaves, so a `focus()`
  into a modal opening with it, or out of one closing with it, holds.
  Either way it beats a restore.
- **Hidden traps.** A trap under `display: none` (Suspense hides this
  way) or `inert` is inactive: no gate, no Tab scope. Its focus goes
  back as on a close, and showing it again activates it and
  auto-focuses, as the web remounts.
- **Scope.** A trap's scope is its subtree plus the layers it owns, and
  theirs: a root-level layer's scope parent is its owner. Owner cycles
  and owners out of the tree own nothing.
- **Nesting.** Tab cycles in the innermost active trap holding the
  focus, else in the top modal, else the window. The top modal is the
  innermost active one, else the most recently activated: a trap
  activating goes below the active traps inside it, so a dialog
  toggled off and on stays under its open menu modal. An inner modal (a menu trap in the dialog's
  layer, made modal) makes the outer dialog's content inert as well;
  closing it gives the dialog back.
- **Modal and hit testing.** The top modal defines a gate: its roots
  (the trap and the root-level layers in its scope) and their tree
  ancestors (the path), both sorted. The hit test restricts where it
  starts rather than checking candidates' ancestors. Under a gated
  parent, a child is classified by binary search: a root is tested
  normally, a path node passes through (children only, never itself),
  and anything else is skipped. With no modal the cost is one bool and
  the `INERT` flag test per visited node, still allocation-free (E15
  below). A press outside the modal keeps the focus.
- **Inert.** A node flag (`NodeFlags::INERT`). The hit test stops at it
  and the point falls through to what is below. Tab, `focus()` and the
  accessibility Focus action skip it, and the accessibility tree drops
  the subtree. Layers an inert node owns escape it, as portals escape
  an inert DOM parent. A hovered node that becomes inert gets its leave
  event on the next frame. A press in progress on a node that becomes
  inert, or falls outside a new modal, is cancelled with #17's
  cancel event (`PRESS` out, no `ACTIVATE`), the pointer press and the
  Space press alike.
- **Owners** (DF-19). The facade's `Layer` reads its owner from React
  context: the nearest `FocusTrap`'s node, else the enclosing layer's
  container. So `<MoreButton/>`'s menu is owned by the dialog's trap,
  and a toast opened in the dialog's layer outside the trap by the
  layer. Layer ops are sent when the transaction seals, after the
  owner's node exists.
- **Tab order** (DF-17). Tree order within the scope, each owned layer
  right after its owner's subtree, unowned layers after the app:

  ```text
  app: Delete…, Save;  dialog layer: Cancel, Delete, More;  menu layer (owner: trap): Rename
  window order: Delete…, Save, Cancel, Delete, More, Rename
  ```

  Each Tab walks its scope once: O(nodes in scope), plus a pass over
  the root-level children for owned layers, sorted by owner. That is
  E15's "Tab" row, the same order of magnitude as before (it collected
  every focusable in the window). A hidden or inert subtree is walked
  only down to owners of layers. Shift+Tab with no focus now goes to
  the last focusable.
- **Accessibility.** Inert subtrees leave the AccessKit tree. Under a
  modal, nodes outside it leave too, and the path nodes above it stay
  as bare containers. An active modal trap sets AccessKit's `modal` on
  its first `dialog` or `alertdialog` descendant, else on itself (ARIA
  puts `aria-modal` on the dialog). Reading a layer next to its owner
  stays topic 13's (DF-17).
- **Removal.** Removing the focused node sends its blur at once, with
  its generation (it used to clear silently). Escape is not handled:
  that is the kit's (a `keymap`).
- **Deferred.** The trap is a layout box (DF-47). `autoFocus` does
  nothing on mount outside a trap, or inside one already holding the
  focus (DF-48).
- **Cost** (E15, exe1, load 12 to 16, no trap open; two runs each of
  main and this branch, interleaved). Hit tests keep 0 allocations and
  stay within noise: at 100k nodes, walk 766 to 1,635 µs against 754
  to 1,540 on main (deep 1,510 and 1,635 against 1,389 and 1,540; an
  earlier run's 3,300 µs deep walk did not repeat), index 5.5 to 61 µs
  against 5.5 to 50. Tab through 1,000 focusables takes one allocation
  fewer; at 100k nodes 585 to 639 µs against 497 to 839. With no
  active trap, Tab skips the scope lookup.

**Built (work item 3, presses and activation).** Innermost press, one
activate, keep focus and focus-visible as targeted (traps, groups and
`inert` are the other half). The example: a pressable inbox row holding an
Archive button, a composer, and a mention button under it.

- A press on Archive sends `PRESS` in and out, then one `ACTIVATE`, to
  Archive alone; the row's `onPress` stays silent, and its raw
  `onPointerUp` still fires. Enter on a focused row activates it on key
  down (each repeat again, as browsers do), and Space on key up, holding
  `_pressed` meanwhile. Modifiers don't stop either, as in Chromium:
  Shift+Enter activates, and Cmd+Enter carries `e.meta` (the kit's web
  Link opens a new tab on it); Space reports the modifiers held at its key
  up. A row `keymap` claiming the chord (Enter, Cmd+Enter) wins, and nothing
  activates. VoiceOver's click activates the row without a hit test, so an
  overlay drawn over it doesn't take the click, and it leaves focus and
  focus-visible alone. It no longer synthesizes a pointer down and up.
  AccessKit's Click is offered only on an enabled pressable, whatever its
  role: a disabled Pressable, or a `View accessibilityRole="checkbox"`
  that only has `onPointerUp`, offers none, and there is no synthesized
  pointer fallback for such nodes.
- The encoding (protocol 7): the INTERACTION flag byte's bits 4 to 6
  carry the node's press flags (pressable, disabled, keep focus; bit 7 is
  reserved). Span flag bit 4 marks a pressable span, the first slice of
  topic 11's interactive spans, and bit 5 (press joins) one that belongs to
  the same pressable Text as the span before it. New events are `PRESS`
  (19) and `ACTIVATE` (20), with listener mask bits 9 and 10. Their key is
  mods | phase or source << 4 | button << 8 | span + 1 << 16, and a span
  comes with the revision of the span table it indexes. x/y are the
  release point for a click, else the node's center. Ops 0x64 and 0x65 are
  still free.
- Only the primary button presses, as only it clicks on the web. A
  pointer that leaves the node keeps the press; the release activates
  when it lands on the pressed node or inside it, else it is `PRESS` out
  with no activate. That is React Aria's rule, not the web's: a browser
  clicks the nearest common ancestor of the press and the release, so
  pressing Archive and releasing on the row body would open the row there
  (`LEDGER.md` DF-45). The activation carries the
  modifiers held at press start, since the platform's pointer-up has
  none: Cmd+click on a row reads `e.meta`.
- Cancel (`onPressOut` with `cancelled`): window focus loss (which also
  ends hover, with leave events), the node leaving the tree, the node
  disabled or unmarked before the release, a new press while one is held,
  or a new span table (a re-render) during a span press, since the pressed
  span may be another Text's by then. That cancel carries the old
  revision, and the facade hands it to the Text that heard the press
  begin. A key press ends when focus moves or the window blurs, for good:
  Tab away and back with Space held, and its key up activates nothing.
- A disabled pressable swallows presses from the pointer and from
  assistive technology: the row around a disabled Archive doesn't
  activate either. Natively it stays focusable if flagged so; the facade's
  Pressable drops `focusable` when disabled. An enabled Pressable inside a
  disabled one still activates, as in React Native; on the web a disabled
  button's content gets no clicks.
- Keep focus (`preventFocusOnPress`, any node on the hit path): the press
  moves no focus, places no caret, and starts no selection. The mention
  button activates while the composer keeps its caret and composition. The
  facade also takes the Pressable out of the Tab order unless `focusable`
  is given, as the kit does on the web (`tabIndex=-1`).
- A Text with `onPress` is a pressable, and only `onPress` makes one:
  `onPressIn` or `onPressOut` alone would have the Text swallow its row's
  presses, so on a Text they need `onPress`. A root Text is one at the
  node level, and reads as a `link` unless given a role. A nested Text
  marks its spans (and the spans inside it), and activates when released
  on any of them: `<Text onPress={go}>See <Text weight={700}>logs</Text>
  </Text>` is two spans, the second flagged press joins, so pressing "See"
  and releasing on "logs" activates it once. Two links side by side stay
  two. One edge: a pressable Text nested in another splits the outer
  one's spans, so a press before the inner link and a release after it
  activate nothing. Neither kind is focusable, like a web span
  (`LEDGER.md` DF-43).
- Focus-visible: the modality model already followed the rules. Keys other
  than bare modifiers and command chords turn keyboard mode on, a pointer
  press turns it off, an input always shows it, and programmatic focus
  keeps the mode. The accessibility click was the gap (its synthesized
  press cleared keyboard mode). One deviation: Chrome keeps the ring when
  a click moves no focus (a keep-focus press); here that press is still a
  pointer press and clears it (`LEDGER.md` DF-44).
- `_pressed` still follows the hit node and its ancestors, like the web's
  `:active`: pressing Archive shows the row pressed too.
- Focus groups activate a member through the same native path
  (`Ui::activate(id, source, mods)`), so `selectOnFocus` needs no event of
  its own.
- The facade (`@craie/bridge`): `onPress(e: PressEvt)` with `e.source`
  (`"pointer"`, `"keyboard"` or `"accessibility"`), `onPressIn`,
  `onPressOut(e: PressOutEvt)` with `e.cancelled`, and
  `preventFocusOnPress`, on Pressable and Text. `onPressIn` and
  `onPressOut` hear the primary button only, where the kit's web versions
  (`onPointerDown`, `onPointerUp`) hear any. `onLongPress` and
  `onMiddlePress` are deferred (`LEDGER.md` DF-42).

**Built (work item 3, focus groups).** As targeted, with these choices.
The example, a size picker:

```tsx
<FocusGroup accessibilityRole="radiogroup" orientation="vertical" selectOnFocus>
  <Pressable accessibilityRole="radio" checked={size === "s"} onPress={() => setSize("s")}>Small</Pressable>
  <Pressable accessibilityRole="radio" checked={size === "m"} onPress={() => setSize("m")}>Medium</Pressable>
  <Pressable accessibilityRole="radio" disabled>Large</Pressable>
</FocusGroup>
```

Tab reaches Medium (checked) alone; the next Tab leaves the group. ↓
focuses Small, skipping Large and wrapping, and activates it (`onPress`
with `e.source` "keyboard"), so Small becomes checked. Shift+Tab out and
Tab back lands on Small.

- **Encoding** (protocol 10; #19 took 9). Op 0x63 `GROUP` (`id u32 | flags u8`:
  `HORIZONTAL`, `VERTICAL`, `LOOP`, `SELECT_ON_FOCUS`); no bits unmake
  the group, and unknown bits fail decoding. Roles 19 `tab` and 20
  `tablist`. Ops 0x64 and 0x65 and interaction flag bit 7 stay free.
  The facade's `FocusGroup` is a View carrying the op (a layout box,
  like the trap: DF-47), and always sends an orientation: `both` by
  default, with `loop` on.
- **Members.** Walked from the group in tree order: a focusable node (or
  input) that is enabled is a member, and its subtree is not searched
  further; a hidden or inert subtree holds none. A disabled one (press
  or state `DISABLED`) is no member and no Tab stop of its own, even when
  left focusable, but what it holds may be members, as on the web. While
  it holds the focus (disabled after focusing, or focused by `focus()`)
  it is a member and the group's stop: Tab and Shift+Tab leave the group
  from it, and an arrow goes to the next or previous enabled member in
  tree order from its place, rather than Tab jumping to the window's
  first stop and arrows doing nothing. A
  `radiogroup` takes only `radio`s and a `tablist` only `tab`s; any
  other focusable in it (a "More…" button) stays its own Tab stop, and
  arrows from it do nothing.
- **The stop**: the member holding the focus, else the first checked or
  selected, else the last focused, else the first. The first rule is a
  choice: with focus on Small while Medium is checked, Tab leaves the
  group from Small, as a roving `tabIndex` does, rather than stepping to
  Medium. A group node left focusable is a Tab stop of its own, before
  its members, and arrows on it do nothing, as on the web: the review
  weighed skipping it or entering at the stop, and kept the web's.
- **Last focused** is native, per group: `Ui::groups` maps the group's
  id to its flags and the (id, generation) of the member focused last,
  noted in `set_focus`. A removed or reused id fails the generation
  check, and removing the group drops the entry. It never crosses the
  wire.
- **Keys.** After claims and Tab, before Enter and Space: the innermost
  group the focus is a member of whose orientation takes the key moves
  the focus; Home and End go to the innermost group's ends. At an end
  without `loop` the key does nothing, and is still the group's (no
  activation). No arrows from a text input, whose caret keeps them, nor
  with Ctrl, Alt or Meta (Shift passes). With `SELECT_ON_FOCUS`, each
  move activates the member reached once, through `Ui::activate` with
  the keyboard source and no modifiers, as the web's `click()`: Shift+↓
  is no Shift+click. A claim on the chord wins over the whole group, so
  JS highlight mode claims ↑↓ and native stays out.
- **`KEY_DOWN` goes to the node focused when the key went down**, if
  still live, before any default action, as on the web: ↓ from Small
  sends `KEY_DOWN` to Small, then blur Small, focus Medium, and (with
  `SELECT_ON_FOCUS`) Medium's activation. This covers every unclaimed
  key, so it changes main in two ways: Tab's `KEY_DOWN` went to the node
  Tab focused and now goes to the one it left, and Enter's activation
  and an input's change or submit now follow the `KEY_DOWN` instead of
  preceding it.
- **Nesting.** A group inside another is one member of it, entered at
  its own stop, and its members are its own: in a vertical group of
  horizontal toolbars, ↑↓ move between the toolbars (landing on each
  one's stop) and ←→ within one. A key the inner group's orientation
  doesn't take goes to the outer group. An empty inner group is no
  member. Focus inside a nested group counts as the outer group's last
  focused too, so Tab back returns to it. An outer `SELECT_ON_FOCUS`
  group's arrow into a nested group activates the leaf it lands on, the
  inner group's stop (a tab holding a toolbar gets selected).
- **Traps.** A group in a trap is one stop of the trap's cycle. A trap
  in a group scopes Tab and arrows to itself: with focus in it, the
  group around it doesn't apply.
- **Accessibility.** `tab` and `tablist` map to AccessKit's `Tab` and
  `TabList`, and a `tab` reports `selected` like a list row. The group
  node reports its orientation, from its flags: horizontal or vertical,
  none for `both` (screen readers announce the arrow axis from it; the
  web sets no `aria-orientation`). AccessKit has no roving focus to
  announce, and each member keeps its own node, role and states.
- **Cost.** Tab walks the tree once. Each group it reaches adds one walk
  of the group's subtree (a nested group's is walked again for each
  level above it), pushing the stops the group takes away. They are
  sorted once at the end, and the order is filtered with a binary
  search, so Tab is O(nodes + skipped · log skipped) however many groups
  there are. The first version re-sorted at each group, and 1,000 groups
  cost 1 to 11 ms per Tab. A `group` bit in the node's `Interaction`
  spares the walks a map lookup on each node that is no group. It isn't
  a node flag because the 16-byte header's flag byte is full. The walks
  of one Tab share their buffers. E15 on exe1, best of 4 interleaved
  runs, load 19 to 47 (single runs varied 2 to 3x):

  | 100k nodes, deep / wide / list | per Tab | allocations |
  | --- | --- | --- |
  | Tab, 1,000 focusable, no group (main: 665 / 700 / 648 µs) | 662 / 807 / 696 µs | 20 / 21 / 24, as main |
  | Tab, 999 in one group at the root | 1,770 / 1,756 / 1,436 µs | 39 / 41 / 45 |
  | Tab, 1,000 groups of 3 (4,000 nodes more) | 1,052 / 1,096 / 863 µs | 35 / 36 / 39 |
  | Tab, 1,000 groups of 3, before the fix | 2,759 / 2,305 / 2,745 µs | about 2,030 |
  | ↓, 999 in one group | 573 / 542 / 526 µs | 12 / 13 / 14 |

  The fix shows best on small trees, where the groups are most of the
  work. At 1k nodes (plus the 4,000), 1,000 groups cost 212 / 154 / 113
  µs per Tab, down from 10.6 / 0.8 / 3.5 ms. The no-group row comes from
  a separate Tab-only A/B, best of 6 alternating runs (load 31 to 52 on
  8 cores, where single runs of either build spanned 650 to 2,600 µs).
  With no group, the walk differs from main by one field read on an
  `Interaction` it already loads (still 12 bytes), so the gap reads as
  load; no instruction counter is on exe1 to settle it. The group map is a B-tree: a `HashMap`
  made Tab in a group 5x slower when the walk looked up every node.
- **Deferred.** No right-to-left flip of ←→ (DF-50). `both` moves in one
  line in tree order, not a grid (DF-51). No PageUp and PageDown, and
  typeahead is JS's, as targeted. Menu, listbox and tree item roles
  wait on DF-41; until then such a group takes every focusable member.

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
  and named scopes (`_row`) through the React tree, up to the nearest
  portal or layer: content there reads scopes inside it only, since the
  hover and focus bits come from the native tree, where it is not inside
  its opener (as on Marbre web). Native keeps a list of dependents per
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

- Four ops in family 0xB0, added in protocol 4: `STATES` (a scope's app
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
  paragraph op, a shape or a layout. Inputs and drawings inherit the
  same way (protocol 5, DF-14 and DF-24 closed): a TextInput's `color`
  is its `COLOR` (INPUT_CONFIG carries none), and a vector shape's
  `currentColor` with no `color` on a `G` above paints the node's
  inherited color, white if none, times the shape's opacity; a
  Vector's own `color` is its `COLOR`. So
  `<Pressable color="#9aa0aa" _hover={{ color: "#fff" }}>` recolors a
  `stroke="currentColor"` icon exactly as its label, with no JS: one
  paint patch per shape, no tessellation (1,000 icons: EXPERIMENTS.md,
  state styles). A `Portal` or `Layer` starts fresh for both inherited
  color and scopes: a Text in a `Portal` under a colored Pressable reads
  neither its color nor its scope.
- The facade (`@craie/bridge`, re-exported by `@craie/react`):
  `defineStates`; `Pressable` and `TextInput` are always scopes, a
  View with `group` (a name makes `_name` address it) is one, and all
  take `selected`, `expanded`, `checked`, `highlighted` and `states`.
  All but `TextInput` take `disabled` (a read-only input is not native
  yet; an input given one logs it and leaves it out). A disabled
  Pressable stops `onPress`, leaves the Tab order and reads as disabled
  to assistive technology. A `_` key is a state of the nearest scope,
  an environment key, or a group up the tree; other `_` keys and every
  value key a variant does not apply (`pointerEvents`, `zIndex`, ...)
  are logged once and left out. Scopes flow through React context,
  which a `Portal` (new: its children are window roots) or a `Layer`
  resets. Otherwise the item in `<Pressable expanded><Layer><Text
  _hover={{ color: "red" }} /></Layer></Pressable>` would turn red with
  the pointer on the trigger, not on the item. The context Provider is
  always there, so toggling `group` keeps the children mounted.
  Variant tables resolve to ids and go out at the seal, when their
  signature changed.
- `_hover` on an element that is no scope means the nearest scope's
  hover, as on Marbre web: in
  `<Pressable><Text _hover={{ color: "red" }} /></Pressable>` the text
  turns red with the pointer on the Pressable's padding. A state key
  with no scope above is logged once and left out; Marbre logs it in
  development (branch `ui/state-scopes`). Marbre's spec still says
  layer content keeps its opener's scope (`ui-kit.md`, and that
  branch's D28 draft); Craie cuts at the layer as web does.
- Layout values apply per key: one per property, axis and side. With
  `padding` 16/12, `_narrow: { padding: { left: 4, right: 4 } }` and
  `_compact: { padding: { top: 6, bottom: 6 } }`, a compact window gets
  4/6, and `_narrow: { height: 44 }` keeps whatever width applies
  (another variant's or the base). Suspense's `display: none` wins over
  a variant's `display`. Border color and width are separate values.
- A variant's `style.transition` times the moves into it, and its
  `animation` runs while it holds (topic 7, work item 6; DF-22 closed).
  Not yet: text metrics in variants (DF-23), a platform source for
  touch and reduced motion
  (DF-25; `Ui::set_touch` and `set_reduced_motion` exist), variants on a
  nested Text (interactive spans, DF-26), z, pointer events and
  visibility in variants (DF-29). Hover can oscillate when a hover
  variant moves the node from under the pointer (DF-27). Transform
  parts, percent translates included, apply per part since work item 6
  (topic 7, DF-21 closed).
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
- Focus traps, `modal` and `inert` are built (topic 3, work item 3).
  A layer opened inside a `FocusTrap` is owned by the trap's node, else
  by the enclosing layer's container, through the same op (DF-19).
  Tab reaches an owned layer right after its owner's subtree; the
  accessibility tree still reads layers after the app (DF-17).

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

**Built (work item 6, transform parts).** Transform parts as targeted;
the animation op, exits, loops and scroll timelines are later parts of
item 6. The example:

```tsx
<Pressable
  style={{ rotate: "12deg", transition: { scale: { duration: 120 } } }}
  _hover={{ style: { scale: 1.02 } }}
  _pressed={{ style: { scale: 0.98 } }}
/>
```

The base sends rotate alone. Hovering tweens scale 1 to 1.02 over
120 ms and the node stays turned 12 degrees. Before, the variant's
matrix replaced the whole transform, so hovering dropped the rotation
(DF-21). Durations are milliseconds, as for every facade timing.

- **Storage.** The spatial row (`host::Spatial`) holds `Parts`
  (translate `[x, y, fx, fy]`, rotate in radians, scale `[x, y]`, the
  free matrix) and a cached `composed = T·R·S·M`. `set_spatial` is the
  one writer: it applies a patch of the changed parts and recomposes
  only when one of them changed. Rotate's sine and cosine snap to 0
  and ±1 within 1e-6, so a half or full turn composes to an exact
  matrix: `animate("rotate", 360)` ends at identity, the node's
  transform record goes (`transformed()` is false), and the subtree
  pixel-snaps again. Unsnapped, f32 `sin(τ)` is 1.7e-7 and the node
  stayed transformed, drawn unsnapped, for good. Validation rejects a
  non-finite part. Finite parts whose product overflows (a 1e30 scale
  over a 1e30 matrix) leave the row as it was, but only the row: the
  base the variants resolve over keeps the part, and each resolve
  drops it again. Rejecting that in validation would need the base,
  and a variant's scale over the base can still overflow later, so
  only the row is guarded.
- **Percent translate.** `fx` and `fy` are fractions of the border box
  (0.5 is 50%), added when the row is read: `Spatial::local(size)`
  adds `fx·width` and `fy·height` to the composed translation, then
  applies it about the center. It is read where the size is already
  known: the scene sync, the hit test, reach bounds, list placement and
  the frame chain. So a percent translate follows a resize without an
  op. Per hit or frame this costs two multiply-adds on top of the
  existing `about`, with no allocation (`harness/invariants`
  allocations test: parts patches, a rotate tween and hit tests through
  the turned node allocate nothing).
- **Encoding** (protocol 9; #18 took 8). No new op: SPATIAL (0x20)
  gains mask bits 3 `TRANSLATE` (4 f32), 4 `ROTATE` (f32) and 5 `SCALE`
  (2 f32), in payload order after the matrix, opacity and z. A part is
  one more optional field of the same row, as opacity is, and an op
  setting several parts stays one op. The variant value mask widens
  from u8 to u16, with bits 8 to 12 for translate x and y (each
  `[points, fraction]`), rotate, and scale x and y. `ANIM_PROP` appends
  `translate` 9, `rotate` 10 and `scale` 11. `transform` (0) now means
  the free matrix only.
- **Tweens.** Each part is its own property, with its own transition
  and running tween, so an `animate("rotate", 360)`, a hover scale and
  a translate transition run on one node at once. Translate lerps all
  four components, so 10 to "100%" passes 5pt + 50%, as a CSS `calc()`
  would. Rotate lerps the angle, so 0 to 360 degrees is a full turn
  that is upside down halfway. Scale lerps both axes. Decomposition
  stays for the free matrix only.
- **Variants.** A variant sets the parts it names, per axis: `_pressed:
  { style: { translateY: "10%" } }` keeps the base translate x, and a
  scale keeps the rotate. Marbre cascades `translateX` and `translateY`
  separately too.
- **Facade keys.** `translate` (a length, "50%", or `[x, y]`; one value
  moves x alone, as CSS), `translateX`, `translateY`, `rotate` (a
  number is degrees, as in Marbre's kit; strings take "deg", "rad",
  "grad" or "turn"), `scale` (a factor or `[x, y]`), `scaleX`,
  `scaleY`. Axis keys override their axis of the pair. `transform`
  stays an RN list. All of these go in `style`, in variants too;
  `transition` and `animate` take `translate`, `rotate` and `scale`.
- **An RN transform list folds into the free matrix.** A list is
  ordered and can repeat steps (`[{ rotate }, { translateX }, {
  rotate }]`), so it has no single value per part. CSS `transform`
  also composes after the individual properties. So `{ rotate: 12,
  transform: [{ skewX: "10deg" }] }` is R·skew, as in a browser. A
  bare number in a list stays radians, as before. It differs from
  `style.rotate` but keeps existing lists unchanged. The angle parser
  now rejects unknown units: "12grad" used to match the "rad" suffix
  and read as 12 radians. Numbers parse strictly (no "", hex or
  "Infinity" before a unit or "%": `Number` read "%" as 0 and "0x10%"
  as 16%). A percentage or string in a list's translate or scale
  throws ("percentages go in style.translate (DF-49)"); it used to
  send NaN, and native rejected the whole transaction.
- **Kit mapping.** Marbre's `transition: { property: "transform" }`
  covers CSS `transform`, `translate`, `rotate` and `scale`. The kit
  adapter should expand it to those four keys. It should also map the
  kit's `translateX`, `translateY`, `scale` and `rotate` style keys and
  the `roll-*` enter presets to parts, not to a `transform` list:
  Marbre's native resolver writes them as a list, with percentages
  (DF-49).
- Tests: `crates/ui/src/parts_tests.rs` checks the composition order
  against a hand-computed matrix, per-part tweens (10pt to 100%
  included), rotate 0 to 360 passing 180, a variant's scale keeping the
  base rotate next to an `animate` on translate, variants merging per
  axis (a hover `translateX` and `scaleX` beside a selected `rotate`,
  then `translateY` and `scaleY`; copying a whole pair fails it),
  exact quarter turns and a full turn ending at identity, percent
  translate after a resize, and hit testing plus drawn bounds of a
  turned and scaled node. `packages/bridge/test/parts.test.ts` covers the keys,
  the per-change diff and the encoding. The cross-language fixture
  carries every part, and the invariants generator sends parts, so the
  incremental-equals-rebuild check covers them.
- **Cost** (E15, exe1, load 22 to 30; main, this branch, main,
  interleaved). 0 allocations everywhere. 100k-node index hit: 5.1 and
  5.1 µs on main, 6.5 µs here. List walk: 1,076 and 851 µs on main,
  785 and 1,342 µs here (two runs), within noise.
- Not yet: percentages inside an RN transform list (DF-49; use
  `translate`).

**Built (work item 6, keyframe animations and variants).** The
animation op, `enter`, `animation`, loops, and motion in variants, as
targeted. Not yet: exits and id parking, scroll timelines, named
keyframes, and blur, shimmer and dash offset frames. The example:

```tsx
<View style={{ opacity: 0.6 }}
  enter={{ keyframes: [{ at: 0, opacity: 0, translateY: 8 }], duration: 200, easing: [0.23, 1, 0.32, 1] }}
  animation={spin && { keyframes: spin, duration: 1000, easing: "linear", iterations: "infinite", reducedMotion: "keep" }}
  _hover={{ style: { scale: 1.02, transition: { scale: { duration: 120 } } } }}
  _streaming={{ animation: { keyframes: pulse, duration: 400, iterations: "infinite" } }} />
```

The row fades in from 0 to 0.6 and rises 8 pt over 200 ms. The spin
runs on the native clock with no render and no op per frame. Hovering
scales it over 120 ms, and leaving hover goes back with the base's
timing (none here, so it jumps). While the `_streaming` variant holds,
native runs the pulse, and it starts and stops with the state and no
JS.

- **The animation.** Frames are ordered offsets `at` in [0, 1]. Each
  holds opacity, the transform parts (translate x and y, rotate, scale
  x and y), `backgroundColor`, `borderColor` or `color`, plus an
  optional easing for the segment that starts at it. A channel a frame
  leaves out interpolates between the frames that set it. The implicit
  frames at 0 and 1 take the underlying value, so `[{ at: 0, opacity:
  0 }]` fades to whatever opacity the node resolves to, and a fade
  toward 0.6 ends at 0.6. Timing follows Web Animations: `delay` (ms,
  negative starts partway through, as CSS), `duration` (ms), `easing` (a CSS name, a Bézier array, `{ steps,
  jump }`, `{ linear: [0, [0.25, 0.75], 1] }` spread as CSS `linear()`,
  or `{ spring }`, which sets its own duration), `iterations` (can be
  fractional, or `"infinite"`), `direction` and `fill`. Box paint on a
  Text throws ("wrap it in a View"): a Text has no box, and so does a
  key keyframes cannot animate (`keyframes cannot animate "bg"`).
- **Triggers.** `enter` goes in the batch that creates the node, and
  native starts it only then, so a later op carrying it does nothing.
  Its fill defaults to `backwards`, so a delayed enter shows its first
  frame through the delay. With `forwards` or `both` its last frame
  holds over the node's value for good, since nothing re-declares an
  enter (DF-58). `animation`'s fill defaults to `none`. The facade
  re-sends a list only when it changes (compared as JSON).
- **Identity.** An entry is its position in the author's list, counted
  before anything is filtered (falsy entries, reduced-motion drops),
  and it travels on the wire. In `[busy && fade, spin]`, `busy` going
  false sends `[spin]` still as entry 1, and the spin keeps its phase.
  A re-sent entry with the same keyframes carries on, taking a new
  timing in place (CSS); changed keyframes restart it (`retargeted`),
  and an entry no longer listed ends (`cancelled`). A finished entry
  stays as a tombstone while it is listed: it covers nothing and isn't
  sampled, and a re-send (a longer list, a reduced-motion flip that
  re-times it) replays nothing and reports no second end. A variant's
  `animation` travels in its VARIANTS entry, keyed by a block number
  the host gives each animated `_` path (`_selected._hover`) when it
  first sees it and keeps for the node's life. So a block coming and
  going (`_pressed: busy && {...}`), before or after, leaves the others'
  loops alone.
  Native starts them when the variant starts applying, and they go
  when it stops.
- **Drawn nodes only.** On a node that isn't drawn (`display: none`,
  under a hidden ancestor, detached) animations don't run, count as
  live, or keep frames coming. The running ones end `cancelled`, and
  when the node is drawn again, all start over, as CSS does across
  `display: none`. Native rechecks when the tree's structure or
  visibility changes. Exits will keep an exiting subtree drawn though
  detached, so the test is "drawn", not "attached".
- **Storage.** Keyframes are `Arc<Keyframes>`, shared. On the wire they
  are interned per transaction: 1,000 rows with the same `enter` send
  its frames once, and each row carries a 42-byte ANIMATION op that
  refers to them by index. Native keeps a `NodeMotion` per animated
  node in `Motion.nodes`: its running list (in composite order), the
  props it covers, and `under`, the values the rows would hold with no
  animation.
- **Composition.** While an animation covers a property, the row holds
  the sample. Every other writer goes to `under` instead: a mutation, a
  variant's value, a transition's tween (`Ui::absorb`). Each frame
  samples over `under`, in this order: enter, then the node's list,
  then variants by specificity. The later one wins per channel. So a
  hover transition on opacity, under a running pulse, tweens the value
  the pulse returns to. When the last animation covering a property
  ends, the row takes `under` back at once. A `forwards` fill holds the
  end over it until the list changes.
- **Variant transitions (DF-22 closed).** The timing of a change comes
  from the style being entered. As in CSS (and Marbre's web kit, which
  emits a whole list per variant), a variant's `transition` replaces
  the node's list while the variant is the most specific active one
  with a list: hover in with the hover's 120 ms, and out with the
  base's. A hover that times only `scale` makes an opacity change
  during hover jump, and `transition: "none"` (or `{}`) in a variant
  times nothing. One difference remains: CSS shortens a transition
  reversed mid-flight; Craie runs the full duration back (DF-60).
- **Loops.** Loops run on the native clock, and a frame touches only
  the nodes in `Motion.nodes`. Nothing is running and nothing is
  stale, so an idle frame returns at once. A loop's frames allocate
  nothing, iteration boundaries included. A spin's boundary shows the
  identity transform, and a pulse's opacity 1. Either would drop the
  node's transform record or opacity layer, change the draw topology,
  and allocate (14 to 30 allocations per boundary, measured). So while
  an animation that isn't done covers a transform part or opacity
  (finite ones too: a 100-iteration pulse has 99 boundaries), `Spatial`
  holds a pin that keeps them (`Spatial::PIN_*`). It goes when the last
  one ends, a real change.
- **Reduced motion resolves in JS.** Each animation and each
  transition entry takes `reducedMotion`:
  - `skip` (the default): a loop is not sent. A finite animation goes
    with delay and duration 0, so its fill holds the end state. That is
    one refinement over "`skip` sends nothing": a `fill: "forwards"`
    reveal still ends revealed. A skipped transition entry drops, so
    the change jumps.
  - `fade`: only the opacity frames go, or skip when there are none.
    Transitions keep only opacity's timing.
  - `keep`: as declared.

  Native sends an ENVIRONMENT event (out kind 21, node NIL, key = the
  environment bits) when the session starts (its first transaction),
  so a host created late learns the setting, and then when the setting
  changes. The facade then re-sends every node's transitions, lists and
  variants under the new setting. Entries keep their indices, so a
  dropped loop moves nothing, and a done one-shot re-timed to 0 s stays
  done. A running `enter` carries on at full length (DF-59). Nothing
  sets the platform bit yet (DF-25).
- **End events.** A finite animation of `enter` or `animation` whose
  node has `onAnimationEnd` reports `{ animation: "enter" |
  "animation", index, finished, reason }`. Here `index` is the entry's
  position in the prop, straight from the wire, and `reason` is
  finished, cancelled (the list dropped it, or the node stopped being
  drawn) or retargeted (its keyframes changed). Loops never end. A
  removed node reports nothing (its handler goes with it). A variant's
  animations aren't reported (DF-56).
- **Encoding** (protocol 11; focus groups, #20, took 10). `KEYFRAMES` 0xA2: a frame count u16, then per
  frame: `at` f32, an easing, a value mask u16 (the variant value
  bits), and the values. `ANIMATION` 0xA3: node id u32, trigger u8
  (enter 0, animation 1), notify u8, and a count u8 of entries, each an
  index u8 (the position in the author's list, rising within a list),
  a keyframes index u16, delay f32, duration f32 (seconds), easing,
  iterations f32, direction u8 and fill u8. An easing is a kind u8
  (0 default, 1 Bézier, 2 steps, 3 linear points, 4 spring) and its
  payload. A VARIANTS entry's mask gains bit 13 TRANSITIONS (count u8 ×
  (prop u8, timing); set with count 0, the variant's list is empty
  and times nothing) and bit 14 ANIMATIONS (block u16, the path's
  number, then count u8 × entry), after the values. Limits
  (native and the facade's encoder check the same): 16 animations per
  list, 256 frames, 256 linear points, delays in ±600 s, durations to
  600 s. Offsets must rise in [0, 1], and an infinite animation needs
  a duration; native names what is wrong ("keyframe offsets out of
  order", "keyframe offset outside [0, 1]", "keyframes without a
  frame", "keyframe value not finite", ...). Ops 0xA4 to 0xAF stay
  free for timelines (exits reuse `ANIMATION`, and add `END_EXIT` next
  to `REMOVE`, below).
- **`animate` stays** for one-off tweens to a target (a drawer's
  offset, a retargeted spring). It resolves a promise, and a keyframe
  animation covering the property plays over it.
- Tests: `crates/ui/src/keyframes.rs` (easings, omitted end frames,
  iterations and directions, fills, springs) and `keyframes_tests.rs`.
  The latter covers enter at creation only, restart on changed
  keyframes only, siblings keeping their phase (list and variant
  blocks), done animations never replaying, drawn nodes only, forwards
  fill over a changing value, variant animations with their variant,
  specificity between two variants, variant transition lists replacing
  the node's, animations over transitions, later wins per channel, the
  pins (loops and finite), negative delays, the ENVIRONMENT event at
  session start and on change, and wire round trips with validation
  messages. `packages/bridge/test/motion.test.ts` covers the example's
  encoding, easings, validation (unknown keyframe keys, delays),
  reduced motion live (skip, fade, keep, and back off), end indices
  and variant paths keeping their blocks. The
  cross-language fixture carries keyframes, both triggers and variant
  motion. `harness/invariants` checks that loop frames (an infinite
  spin, a 100-iteration pulse) allocate nothing on and off their
  boundaries, and a GPU test checks that a looping
  rotation turns the pixels at 0.25 s and 1.25 s with no transaction.
- **Cost** (`harness/invariants/examples/motion_cost.rs`, exe1, release,
  after review #21, three runs at load 47 to 48, so noisy). Mounting
  1,000 rows (a View and a label), median of 15: 33.8 to 51.3 ms plain
  and 40.8 to 59.0 ms with `enter`, within noise of each other (at
  load 43 before the review: 11.2 and 12.4). Wire: 85,204 bytes, and
  127,226 with `enter` (a 42-byte ANIMATION op per row). Per frame:
  1,000 enters run 0.47 to 1.77 ms. One loop among 1,000 still rows
  takes 0.09 to 0.19 ms, 1,000 loops 0.53 to 1.28 ms, and a still
  frame nothing. Loops allocate 0 per frame. The enters allocate about
  13 times over their 22 frames (0.6 a frame), then nothing.
- Not yet: scroll timelines and named keyframes (the rest of item 6;
  exits and id parking are built, below). Also blur and shimmer frames (work item 7), a
  dash offset channel (DF-55), end events for variant animations
  (DF-56), an `enter` fill that can't be let go (DF-58), a running
  enter under reduced motion (DF-59), and CSS's shortened reversals
  (DF-60).

**Built (work item 6, exits).** `exit`, id parking and the size
channels, as targeted, except that the exit travels in the `ANIMATION`
op just before the `DETACH`, not inside it. The example:

```tsx
{toasts.map((t) => (
  <View key={t.id} style={{ overflow: "hidden" }}
    enter={{ keyframes: [{ at: 0, opacity: 0, translateY: 8 }], duration: 200 }}
    exit={{ keyframes: [{ at: 1, opacity: 0, height: 0 }], duration: 200, reducedMotion: "fade" }}>
    <Text>{t.text}</Text>
  </View>
))}
```

Dropping toast B from `[A, B, C]` sends, in one transaction,
`ANIMATION(B, exit, [fade and collapse])` then `DETACH(B)`. For the app
B is gone. Natively it stays between A and C, drawn, and fades while its
height goes from 40 to 0, so C slides up through ordinary layout: when
B is 20 high, C sits 20 higher. At 200 ms native frees B and
its text and sends one `exitEnd` (B, finished). Only then does the
facade reuse B's two ids.

- **Wire.** Protocol 12. `ANIMATION` trigger 3 declares an exit,
  which starts on the node's next `DETACH`. The facade resolves
  `exit` under the reduced-motion setting of the moment and sends it
  right before the detach, so nothing is declared ahead and an exit
  never goes stale. Its entries carry their index in the prop, like
  any list's. `EXIT_END` is out kind 22: node = the exit's root,
  generation = the one it had, key = the reason (finished, removed,
  parent gone or skipped; the animation reasons' codes 0, 3, 4 and 5).
  The session never drops one, since a lost end would leak ids. Frames
  gain `width` (bit 13) and `height` (bit 14), border-box points, for
  exits only, and a size frame anywhere else, or an infinite exit, is
  rejected by both sides. One new op, `END_EXIT` (0x05, a node id),
  ends an exit if it still runs, and does nothing once it has ended:
  see Ids.
- **From what shows.** An exit's omitted frames hold what showed at the
  detach, for each channel it animates (Framer's `AnimatePresence`
  does the same): the laid-out size, and an enter, a transition or a
  hover animation cut short where it stood. What ran under the exit
  moves on unseen. Toast B, fading in over 1 s and removed at 200 ms
  with the fade out above (500 ms here), shows 0.2 → 0.16 at 300 ms,
  where following the enter would have shown 0.24.
- **Inert, not gone.** The root stays in its parent's child list with
  `inert` set (topic 3), so the subtree has no hit testing, focus or
  AccessKit node. A focus inside moves on as for a removal, a trap or
  focus group inside stops counting, and its text leaves the text
  selection: a selection inside drops, as at a plain detach, and one
  around it no longer copies or highlights it. Only the removed root's exit
  runs: its descendants are not detached, so theirs never start, and an
  exit already running inside (removed earlier) ends with the outer one
  as parent gone.
- **One end per exit detach, always.** Native answers every exit
  detach with exactly one `EXIT_END`: when its animations' active
  phases end (then it frees the subtree); at once when JS cuts it
  short with `REMOVE` or `END_EXIT` of the root (removed); at the end
  of the transaction when an ancestor was detached or removed (parent
  gone), or when the exit couldn't run, its root being out of the tree
  or a List row (skipped). So does one hidden (`display: none` on it or
  above), at the detach or by a later transaction, whether frames are
  drawn or not (a minimized window draws none), judged after the
  transaction's last restyle (a `_focusWithin` variant that showed it
  hides it once the detach moved the focus out): #21 parks the
  animations of undrawn nodes, and a parked exit would hold its ids for
  good. A variant that hides it between transactions (hover, focus)
  ends it on the next frame. A visible exit in a window that draws no
  frames (minimized, occluded) waits, as tweens do: it ends on the
  first frame after the window shows again. An exiting subtree counts
  as drawn (its root stays in its parent's list), so loops inside keep
  running through the exit. An exiting node never comes back:
  validation rejects placing it, placing under it, or detaching it
  again.
- **Validation models it.** Structure alone decides at the detach
  whether an exit runs (out of the tree or a List row: skipped), so
  validation knows the root keeps its parent and position: a sibling
  placed before it later in the same transaction is accepted, as it is
  in the next one. A `REMOVE` or `END_EXIT` of an exit's root frees its
  whole subtree in validation as in execution, so a later op naming a
  node inside (`REMOVE(B), PLACE(root, B's text)`) is rejected before
  anything applies. Each cut walks only its subtree: the host's
  children and the batch's own placements, indexed by parent at the
  batch's first cut and kept as it places. Unmounting 1,000 exiting
  toasts looks at 3,000 nodes and links, not half a million. Ending
  them is linear too: the running exits are a sorted set, and the
  events queued for freed nodes are filtered once, when events are
  next taken, not at each cut.
- **Ids.** React detaches the removed root, then releases every node of
  the subtree. With an exit running, the facade parks each released id
  on the exit instead of sending `remove`, and recycles them all on the
  end event (a stale generation is ignored). A release that comes after
  the end recycles at once, with no op. A layer whose last child is
  exiting stays open until the end. Unmounting the root sends no new
  exits, and sends `END_EXIT` for each running one so they end at
  once. Not `REMOVE`: native may have finished the exit and freed its
  ids with the end event still on its way, and a `REMOVE` of a freed id
  is an error that closes the session. `END_EXIT` then does nothing,
  and a second unmount sends nothing.
- **Layout.** The node keeps its child position and z. A new sibling
  placed before C lands between B and C (`[A, B, D, C]`). There is no
  implicit clip: the app sets `overflow: "hidden"` for content not to
  spill while the box shrinks. Padding, border and a min size floor the
  collapse, and a parent's gap stays until the end (DF-62). List rows
  skip their exit (DF-61): the list windows its rows itself.
- **Events.** While an exit runs, native drops the events of its
  subtree's nodes, and at the free it drops their queued ones (tween
  and animation ends, a blur). The facade also forgets the subtree's
  nodes as React releases them (the same commit), so a late event finds
  no handler.
- **Reduced motion** at the moment of removal: `skip` sends no exit and
  the subtree goes at once, `fade` keeps the opacity frames only (no
  collapse: the gap closes at the end), and `keep` runs it as declared.
- **Cost** (`harness/invariants`, `exit_transactions_allocate_twice`,
  measuring the bridge's whole transaction, `ANIMATION` then `DETACH`).
  The transaction allocates twice: validation's overlay entry, which
  any detach pays, and the declaration's copy. The animation record
  reuses an emptied record's list (keyframes keep up to 64). A plain
  detach allocates 3 here: the overlay entry, plus 2 as its parent's
  child list moves, which an exit's root, staying, skips. The render
  allocates once more than a plain detach's: the fade's opacity layer,
  which #21's pin builds up front. Its frames then allocate nothing. A
  size frame relayouts each frame and allocates in layout, as a
  `height` tween does (not asserted). The end allocates no more than
  removing the same subtree plainly (8 against 9; one of the 8 is the
  list of freed nodes whose queued events go when events are next
  taken, PR22-09).
- Tests: `crates/ui/src/exit_tests.rs` covers the subtree drawn, laid
  out and inert; a collapse moving the next sibling up, then the free
  with one event; placement beside an exiting node, in the detach's
  transaction too; parent gone, removed (a skipped one too) and
  skipped; `END_EXIT` cutting a running exit and doing nothing after
  its end; a hidden exit skipped (before and during) with no frame
  drawn; the start from
  what showed (an interrupted enter, a transition, a hover animation);
  loops inside running through it; a descendant's exit not running and
  an inner exit ending with the outer; focus leaving at detach; a trap
  inside releasing; validation, a cut exit's subtree freed in it (a
  node placed after an earlier cut too), a bulk cut's work (1,000
  cuts, each subtree walked once), and the wire round trip.
  `packages/bridge/test/exits.test.ts` covers the example's ops, ids
  parked until the end then recycled, removes not freeing early,
  dropped events, the three policies, unmount (twice, and with an end
  still on its way), a layer held open, and a release after the end.
  The cross-language fixture carries an exit with width and height
  frames, and an `END_EXIT` of a node with none.
- Not yet: exits of List rows (DF-61); sizes from `auto` or in
  percent, and size frames in `enter` (DF-63).

## 8. Paint and text styling

Changes: §3 (paint store), §5, §8, §11, §15 (cursor in the platform
contract).

**Target.**

- **Box paint**: a shadow list, a border width for each side (solid or
  dashed), a radius for each corner, and a fill from a paint source. A
  shadow layer has x, y, blur, spread, color and inset, and layers draw in
  order; the kit already sends them structured (`elevation.card` is a ring
  layer plus a shadow stack). Built (protocol 14, `shadow.rs`): the
  shadow list, up to 8 layers, as React Native's structured `boxShadow`
  on every box and in variants; analytic (P1's first option), checked
  against the Gaussian on the GPU. Shadows snap under a transition for
  now.
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
  Balanced and pretty wrapping come later. Built (protocol 15): text
  alignment (`textAlign`: auto, left, center, right; justify draws as
  auto) and tabular digits (React Native's `fontVariant`).

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
  view box and up to 4,096 shapes of 45 bytes (kind, fill rule, join,
  cap, `current` flags, three string refs, fill and stroke colors,
  width, miter limit, dash offset, opacity; protocol 5 added the
  flags). The strings stay SVG syntax and native parses
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
- Paints are plain colors, or the node's inherited color. The facade
  resolves `currentColor` to the `color` of the nearest `G` above, as
  SVG does; without one it sets the shape's `current` flag for that
  paint and sends a tint (white, alpha the paint's opacity), and native
  paints `COLOR` (the Vector's own, else the nearest ancestor's, else
  white) times the tint (topic 5; DF-14 closed). Each such paint has its
  own slot in the node's chunk, so a color change patches slots and
  tessellates nothing. `fillOpacity` and `strokeOpacity` scale the
  alpha. Gradients stay build-time.
- The facade flattens children into shapes: `Path`, `Circle`,
  `Ellipse`, `Rect`, `Line`, `Polyline`, `Polygon` and `G` (attributes
  inherited, transforms nested, a `G`'s opacity multiplied into its
  shapes: DF-16), with the Vector's own paint props as defaults, as on
  an `<svg>` element. The Vector's `opacity` is the node's: one layer,
  animatable. Numbers are coerced as SVG reads attributes; a shape with
  one that is not finite is dropped, with a warning. The kit's shapes
  differ in two ways: token colors must be resolved first, its
  `Path` and `Circle` wrappers use hooks and so throw (shapes must be
  direct elements, DF-15).
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
  keeps it too (DF-34, DF-37). Once the box holds for a frame, a bitmap
  of another size decodes once more at the drawn size. A new `src` keeps the old image and its
  natural size until the new pixels or failure (DF-38).
- The platform decodes on one worker thread (`image` crate: PNG, JPEG,
  WebP, GIF's first frame; EXIF orientation applied) and averages the
  crop down, premultiplied. It rejects images over 64 megapixels at the
  header and reserves its buffers against 512 MiB before decoding (a
  250 KB PNG can claim 16,000 x 16,000); a WebP chunk that claims more
  than the file holds skips the EXIF read (image-webp would allocate
  the claim). Probes jump the queue, queued
  work for dropped images goes, and a codec panic fails only its image.
  The core keeps the pixels (64 MB for all images, then decode again)
  and puts them in the color atlas as a raster; the quad is a color
  glyph, so images need no new instance type or shader. The gutter
  repeats a color raster's edge pixels, so magnified edges keep their
  color. No color management yet (DF-35).
- The facade fetches URLs (http, https, data, blob, file, paths) once
  per `src`, or takes a `Uint8Array`, and keeps the old image until the
  new one arrives, and takes state styles (`_pressed`) like any host
  element. A failed or timed-out (30 s) fetch clears the image
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

**Current.** Roles up to `switch`, `radio` and `radiogroup` (protocol
6). States come from a scope's bits. The check roles (`checkbox`,
`switch`, `radio`) always report `checked` as AccessKit toggled, so
`<Pressable accessibilityRole="checkbox" checked={on}>` reads "checkbox,
unchecked" until `on`; it takes a click as any enabled pressable does
(topic 3). `expanded` and `selected` are
reported where the prop was given: the ROLE op carries a `reported`
byte the facade sets from `props.expanded !== undefined` and
`props.selected !== undefined` (a bit alone can't tell `false` from
absent), so `expanded={false}` is collapsed in the tree and a plain
button is neither. Only the Windows and iOS adapters read `expanded`:
on macOS and Linux a menu trigger is a plain button (`LEDGER.md`
DF-40). `selected` is reported on selectable roles only, as Marbre web
gates `aria-selected`: of Craie's roles, the list row. The kit styles
checkboxes and radios with `selected`, and they must not read "checked,
selected". `checked` on other roles stays a styling state; `disabled`
reads disabled. Every transaction republishes the whole tree, so a
state change reaches the next update; the semantic revision these bits
bump is bookkeeping, as for labels. Not yet: pressed, mixed,
highlighted and the menu roles (DF-41), and the other states below.

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
- Built (protocol 13, `observe.rs`): layout events (border box relative
  to the parent, after layout, on change), `measure()`, and window state
  with size, scale, focus, visibility and dark appearance; the pointer
  kind, contrast and font scale wait for platform sources. Also
  `presented()` and `capture(path)`: a promise on the frame that shows
  the commits made so far, at rest if asked, written to a PNG on request.
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
   `inert` and item 4's owners. Done: #17 (presses), #18 (traps), focus
   groups.
4. Sibling z and the sorted child order; layer containers with owners
   (topic 6, first half).
5. State styles: scopes, variant tables with layout values, specificity,
   inherited color, hover at rest (topic 5). Item 6 needs it, because
   animations live in variants, and item 9 needs it for placement states.
6. Motion: the animation op, exits, transform parts, scroll timelines
   (topic 7). Done except scroll timelines: #19 (transform parts), #21
   (animations), exits.
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
| O2 | Geometry | The expression grammar, and its mapping to CSS Anchor Positioning and Floating UI on web |
| O3 | Shaders | One translated shader source or one per platform |
| O4 | Shaders | The GPU budget's value and scaling |
| O5 | Tables | When to window the column axis |
| P1 | Paint | The shadow rendering technique (experiment) |
| I2 | Inline | The rewrap budget for chips that change size (experiment) |
| — | Kit | The namespace rule for plugin state names; whether toasts draw above dialogs |

Closed in review: K1, K2 (topic 2), F1 (topic 3), S1, S2 (topic 5), M1,
M2, M3 (topic 7), I1 (topic 11), L1 (topic 6), V1, V2 (topic 12).
Closed in implementation: O1 (topic 3, work item 3).
