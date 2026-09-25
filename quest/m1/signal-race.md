# [S] Signal.race releases its listeners when it loses a race

## Goal

`Signal.race` from `@moq/signals` no longer leaves a `changed` listener on
each signal when its own promise is raced and loses. Today it disposes them
only when one of the signals changes, so `js/net/src/origin.ts`'s
`#changed()`, raced against `closed` in `connection/forward.ts`, keeps
listeners for as long as the table stays quiet. Awaiting it directly still
works, with the same signature.

## Plan

Make it consistent with the free `race()` and `effect.race` from
[JS retention](https://github.com/moq-dev/moq/pull/4085), which already
subscribe to `Once`/`GetPromise` values and dispose them when the race
settles. `Signal.race` returns that same kind of awaitable instead of a
native promise: it attaches its signal listeners when first awaited or
subscribed, and releases them when it settles or when its last subscriber
detaches. Racing it through `race()` or `effect.race` then cleans up both
sides. Settle the exact return type at PR time, keeping `await` source
compatible; if the change is a published type break, stop and bring it back.

Audit the callers in `js/net` (`announced`, `broadcast`, `group`, `origin`,
`track`, both subscribers) for the ones that race the result, and add a
listener-count test for the `origin.ts` case that fails today.

## Required

- JS retention (#4085) merged, which adds the free `race()` this builds on
