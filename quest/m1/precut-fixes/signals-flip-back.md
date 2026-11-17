# [S] A signal set and set back before it notifies reaches every reader

## Goal

In `@moq/signals`, a reader that registered and read a signal's value while a
notification was pending is notified whenever the value it saw differs from
the final one, so a signal flipped and flipped back within one batch never
leaves an effect or `changed()` waiter stuck on the middle value.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; reproduced on `main` with
a bun test: an effect reading `t` and `s`, then `t.set(1)`, then
`queueMicrotask(() => s.set(0)); s.set(1)`. The effect sees `[0, 1]` and
never reruns, while `s.peek()` is 0. `set` (`js/signals/src/index.ts`) saves
the value it replaces, and the queued notification is skipped when the final
value equals it, though a listener added in between saw the middle value.
On `release` too, from the original batching design.

Recommended: record the value each listener added during a pending
notification saw, and deliver to it when the final value differs. No API
change, so it does not touch [signal semantics](/quest/m2/signals-semantics.md)'s
frozen shape. Add the repro as a test.

Public API: none. Wire: none.

## Related

- [Signal semantics](/quest/m2/signals-semantics.md) - changes `update`, `mutate`, and `set(v, false)`
