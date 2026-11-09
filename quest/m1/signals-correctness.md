# [M] @moq/signals delivers every change exactly as documented

## Goal

`@moq/signals` delivers each change to every listener, in every case the
audit found broken:

- `useValue` re-renders after `mutate()` and on a `Derived` that builds
  objects, without React's "Maximum update depth exceeded".
- A silent write (`set(v, false)`) never stops a later real change from
  being delivered.
- Comparing cyclic or very deep values never throws inside the flush.
- Disposing a `watch()` right away cancels its first delivery.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`):

- Milestone: ✅ m1 (rejected: m3), audit scores 6, 5, 4 and 4/10.
- Equality: ✅ past a fixed depth, values compare unequal (rejected:
  tracking visited pairs). Reason: it costs nothing on the common path,
  covers deep acyclic data too, and an extra notification is always safe.

Fixes, all reproduced at `9f51fa0966`:

- React (`js/signals/src/react.ts:11-17`): the snapshot is `peek()`, and
  `Derived.peek()` (`index.ts:1251-1253`) recomputes on every call.
  Recommended: snapshot a version number bumped in the subscribe callback,
  and return the delivered value, as `solid.ts:19-20` already does. Add
  tests for `Derived` and `mutate`. No in-repo code imports the adapter, so
  check the README and `doc/lib/js/signals.md` examples still hold.
- Silent write (`index.ts:100-108`): the old value is captured before the
  `notify === false` return, so the drop-when-unobserved path
  (`:115-119`) never runs. Sequence: no listeners, `set(1, false)`, a
  listener reads 1, then `set(0)` compares the saved 0 with 0 and delivers
  nothing. Move the return above the capture, and fix the property-test
  model (`properties.test.ts:132-134`), which copies the bug.
- Equality (`index.ts:1304-1328`): `isEqual` recurses with no bound, and
  the `RangeError` escapes the flush microtask (`:143`) uncaught.
- `watch` (`index.ts:203-207`): the first call is queued separately from
  the subscription, so dispose doesn't cancel it. It also delivers twice when
  a `set()` lands before it runs. Make dispose cancel it, and fix the
  docstring, which says "now".

Public API: none. Wire: none.
