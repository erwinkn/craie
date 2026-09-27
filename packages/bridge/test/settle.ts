// Waiting for a commit in tests.

const tick = () => new Promise(r => setTimeout(r, 0))

/** Ticks until `done()` holds (at most 100 ticks), then once more, so
 * a frame sent just after shows too. React commits a state update
 * through its scheduler, and hidden work at idle priority: on a loaded
 * machine, later than one tick. */
export async function settle(done: () => boolean) {
  for (let i = 0; i < 100 && !done(); i++) await tick()
  await tick()
}
