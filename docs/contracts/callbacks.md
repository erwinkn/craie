# Contract: callback refs (native → JS calls)

Agreed 2026-10-04 by Craie A (native, wire, bridge) and Craie B (the kit),
through the coordinator. The protocol number is assigned when it lands.
Companion: `lists.md`.

## The rule it keeps

Native never waits for JS. Every call native makes into JS is
asynchronous. When JS has something to say back, it says it the way a claim
answers today: as an op in a later transaction.

## What a ref is

A callback ref is **(owner node, slot)**. It is not a fresh id per function.

- **Owner.** The node whose props held the function, or NIL for window-level
  callbacks (window hotkeys today).
- **Slot.** A u8 that names which prop it is. Slots 0–31 are typed: today's
  listener mask bits (pointer, key, focus, input, scroll, press, activate,
  layout), plus slots each component defines (a list's `onRangeChange`,
  `onViewportChange`, `loadItems`; see the list note).
- **Declaring.** JS serializes props by stripping each function and setting
  its slot's bit in a u32 mask (`INTERACTION`'s listener mask, widened to mean
  "live slots"). Only the presence of a function crosses the wire, never the
  function.
- **Resolving.** JS keeps `(node id, slot) → current prop value`. Arguments
  are decoded and passed to whatever function the prop holds *now*. So a
  re-render with a new closure sends nothing, because the bit is unchanged.
  That is what React DOM's event delegation does, and it is what Craie does
  today for event handlers. A per-closure ref would cost an op per render per
  handler.

### Example

`<Pressable onPress={() => open(id)}>` declares bit `ACTIVATE` on its node.
Ten re-renders with ten new closures send nothing. A press sends `ACTIVATE`
(node, generation, …), and JS calls the closure the props hold at that
moment.

## Lifetime and release

- **Set or clear.** A slot is live while its bit is set. When a prop goes
  from a function to `undefined`, the bit clears in that commit's
  `INTERACTION` op. Native stops calling it from that transaction on.
- **Owner gone.** A node's removal frees all its slots, natively and in JS.
  There is no release op to send or to forget.
- **Race.** A call already in flight when JS released the slot, or when the
  node id was recycled, carries the node generation. JS drops it, the same
  generation check events use today.
- **Versioned slots.** A slot whose answer must match what was on screen
  carries a version, the way claims do today (topic 2 of the architecture
  update). Declaring a new version retires the old handlers on that
  transaction's ack. The claim set stays the model for versioned slots, and
  only claims need it so far.

## A call on the wire (native → JS)

- **Typed slots keep today's 36-byte event record:** node, generation, kind,
  x, y, a, b, key, revision, text. No bytes change for existing events.
- **Generic slots use a new event kind `CALL`.** Its record holds:
  - node and generation;
  - `key` = slot | call id << 8 (the call id is a u24, 0 = no reply wanted);
  - `text` = the arguments as a byte payload, a small tagged list: nil, bool,
    i32, f64, utf-8 string, u32 array, f64 array, or nested list. Each element
    is a tag byte then its value; no names; the slot's TypeScript signature
    gives the meaning.
- **Ordering.** Calls arrive in the event stream in native order, between
  acks as today. A call raised before a transaction applied comes before
  that transaction's ack.
- **Delivery.** Every slot's spec names its delivery class:
  - **Reliable:** never dropped, delivered in order. Used for `loadItems`
    and for claims.
  - **Every call:** each call is delivered and none is merged, but they may
    drop under queue pressure, like pointer events. Used for
    `onRangeChange` and the typed input events.
  - **Latest wins:** calls coalesce per frame, and only the latest
    arguments are delivered. Used for `onViewportChange` and frame
    statistics.

  A slot spec states its class next to its arguments. The lists contract
  lists its four slots.

## Answers (JS → native)

A slot whose spec asks for an answer (for example, a list's `loadItems`
answering with descriptors) gets it through an op in a later transaction:

- `ANSWER (call id u24, status u8: 0 ok / 1 error)` followed by the slot's
  answer payload, which has its own typed op.
- **Native keeps working meanwhile.** It treats the request as pending: a
  list shows its estimates, or the placeholder rows JS already rendered. A
  later answer for a call native has since cancelled (its node removed, the
  range scrolled away) is dropped, never applied twice. A newer call's
  answer wins over an older one.
- **Timeouts.** There are none. A pending call is simply superseded or
  cancelled.

## Errors

- **A throwing callback** is caught by the bridge and reported through
  `onError` on the root (`console.error` by default). If the slot expects an
  answer, `ANSWER` is sent with status error and native keeps its fallback.
- **A call for an unknown slot or a stale generation** is dropped silently, as
  today's events are.
- **JS sending `ANSWER` for an unknown call id** is a no-op, not a validation
  error. The call may have been cancelled.

## How today's handlers move onto it

- **Pointer, key, focus, input, scroll, press, activate and layout events**
  are already typed slots: the mask bits and the kinds stay. The bridge's
  dispatch becomes one table lookup `(node, slot) → prop` instead of a switch
  per kind. Behavior is unchanged.
- **Claims** stay versioned slots. Their events and answers (`InsertText`,
  `WriteClipboard`) are the model `ANSWER` generalizes. They migrate later,
  unchanged in meaning.
- **`measure` and `presented`** (protocol 13) are request/reply already. They
  stay as they are, since they are commands from JS with native answers,
  the mirror case.
- **New component callbacks** (lists first) use generic `CALL` slots.

## Not proposed

- **Synchronous calls** in either direction.
- **Passing functions as arguments** or returning functions.
- **Shared memory.**
- **One id per closure.**
