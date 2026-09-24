# [L] A long-running JS player or publisher keeps a flat heap

## Goal

No `js/` package retains a listener, promise reaction, or task per frame or
group. A four-camera player measured over minutes today grows from about 715k
to 2M heap nodes and doubles its major-GC pauses (#4024, #4025, #4026); after
this quest its heap stays flat for the life of a subscription or effect.

The cause is one pattern: a per-frame wait that attaches to a value living as
long as the track or effect run, and never detaches.

- `Promise.race` against a pending `Once` (`track.closed`, `producer.closed`)
  registers a listener per call through `Once.then`, released only when the
  track closes (#4024, lite and IETF subscribers).
- `Promise.race` against a pending native promise (`effect.cancel`, `Sync`'s
  update promise) adds a reaction per call, released only on rerun (#4025).
- `Effect.spawn` keeps every settled task on an effect that never reruns, one
  per group in `@moq/hang`'s container `Consumer` (#4026).

## Plan

Settled with the maintainer:

- Add `effect.race(promise)` to `@moq/signals`: resolves with the promise's
  value, or `undefined` once the current run tears down, and removes its
  teardown listener either way. One promise; a site racing several combines
  them with `race` first. A teardown that wins must also dispose the inner
  `race`'s subscriptions, or each run leaks them; settle how at PR time.
- Export a free `race(values)` from `@moq/signals`, shaped like
  `Promise.race`: it accepts promises and `GetPromise` values, subscribes to
  pending ones (an already settled one wins at once), and disposes every
  subscription when the first settles. A native promise's reaction cannot be
  removed, so a caller never passes one that outlives the call. It sits beside
  `Signal.race`.
- Every internal `effect.cancel` race moves to `effect.race`, and
  `effect.cancel` is marked `@internal`, per the `js/CLAUDE.md` deprecation
  convention. Its deletion is a published break,
  left to [Remove effect.cancel](/quest/m1/effect-cancel.md). Both additions
  are additive, so this quest targets main.
- `Effect.spawn` drops a task once it settles; a rerun still waits on pending
  ones, and close still releases them without waiting.
- Audit every `Promise.race` and `Once.then` under `js/`, not only the reported
  sites: signals, net, hang, watch, publish, room, and the rest. Convert each
  race whose operand outlives the call. `Sync.wait` is internal, so it can wake
  on signal changes it disposes, as the reporter's proxy does, rather than a
  shared native promise.
- The reporter still saw about 48 `Promise` objects/s growing after their
  patches, with reactions and closures flat (#4025). Find and fix it.
- Add a rule to `js/CLAUDE.md`: never race a value that outlives the call; use
  `effect.race` or `race`. Read `PROMPTING.md` first and keep it one line.
- Update `doc/lib/js/signals.md` and the `effect.cancel` example in
  `doc/lib/js/watch.md` inline.

Tests, in per-PR CI:

- Per site, a deterministic test that many frames or groups leave the
  long-lived value's listener count unchanged, and for `race` and
  `effect.race` themselves.
- One heap backstop: a long mock subscription through the player path under
  forced GC, asserting heap does not grow with the number of frames.

Reproduce each leak before fixing it. The issues carry measured numbers and
the reporter's local patches, useful as a reference but not as the design.

## Closes

- [#4024](https://github.com/moq-dev/moq/issues/4024) - close this issue when the quest finishes
- [#4025](https://github.com/moq-dev/moq/issues/4025) - close this issue when the quest finishes
- [#4026](https://github.com/moq-dev/moq/issues/4026) - close this issue when the quest finishes
