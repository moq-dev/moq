# [M] A signal notifies on a change, however it was written

## Goal

In `@moq/signals`, `Signal.update(fn)` notifies only when the value changed,
exactly as `set(value)` does. `mutate(fn)` always notifies and takes no
flag. The silent write, `set(value, false)`, and `Effect.timeout` are gone.
One spelling forces a notify, for the few callers that publish the same
value on purpose. The rest of the package's shape (`Signal`, `Computed`,
`Derived`, `Once`, `Effect`, `Getter`, `Setter`) freezes as it is.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `update` compares like `set` (rejected: documenting that `update`
  forces; keep). Two writes of the same value should not differ by which
  method wrote it.
- ✅ `mutate` always notifies and loses its parameter (rejected: keep the
  flag). A mutation in place cannot be compared, so notifying is its only
  correct behaviour.
- ✅ The silent `set(value, false)` goes (rejected: keep). Nothing outside
  the package's own tests uses it.
- ✅ `Effect.timeout` goes (rejected: keep). It has no caller, and it reads
  as `setTimeout` beside `Effect.timer`, which is that.

Facts found by the audit, re-checked at `80ca9787e3`:

- `set(value, notify?)` (`js/signals/src/index.ts:100`) notifies on a
  change when `notify` is omitted, always on `true`, never on `false`.
  `update(fn, notify = true)` (`:196`) and `mutate(fn, notify = true)`
  (`:205`) pass `true`, so both force: `s.set(1)` on a signal holding 1
  delivers nothing, while `s.update((v) => v)` delivers once.
- `grep -E '\.update\('` finds 185 call sites outside tests in `js`,
  `demo`, and `test` (some are not signals); none passes a second
  argument. `.mutate(` has 65, none with a flag.
- `set(value, false)` on a `Signal`: only
  `js/signals/src/index.test.ts:143` and `react.test.ts:206`. The
  three-argument `effect.set(signal, value, cleanup)` is a different
  method and stays.
- `set(value, true)` outside tests: `js/net/src/track.ts:4057`,
  `js/net/src/ietf/publisher.ts:963`, `js/watch/src/broadcast.ts:589`,
  `:590`, `:651`, `demo/boy/src/game.ts:225`,
  `demo/carla/web/src/truck.ts:295`, `:299`. The demos send a repeated
  command on purpose.
- `Effect.timeout` (`index.ts:876`): one caller,
  `js/signals/src/index.test.ts:1045`.

For the PR:

- The risk is an effect that relies on `update`'s forced notify, such as
  `update((list) => { list.push(x); return list; })`, which returns the same
  object and would now deliver nothing. Before changing the method, find
  them: make `update` throw in tests when `fn` returns its argument
  unchanged, run every JS package's tests and the browser interop lane, and
  move each hit to `mutate`. Record the count in the PR.
- Keep one forcing spelling. Recommended: `set(value, true)` stays as the
  only use of the second parameter, typed `true` so `false` no longer
  compiles (the audit's pick); a named method is the alternative if the
  boolean reads badly. `js/AGENTS.md` names `set(v, true)`, so update that
  line if the spelling changes.
- Not in scope, noted by the audit: `Effect.animate` hands its callback a
  bare `DOMHighResTimeStamp`, the one bare millisecond number left in the
  core packages, with about six callers. Leave it unless the PR finds it
  free.
- `@moq/signals/react` and `@moq/signals/dom` are unchanged.
- Update `js/signals/api/*.api.md`, the package's `MIGRATE.md`, and the
  Signals section of `js/AGENTS.md`.

Tests: `update` returning an equal value delivers nothing and returning a
different one delivers once; `mutate` delivers once per call; a forced write
of an equal value delivers once. Verify playback and publish in a real
browser with `just dev`, since a missed notify shows up as a stuck UI, not a
failed unit test.

Public API: breaking for `@moq/signals`: `update` stops notifying on an
equal value, `update` and `mutate` lose their second parameter,
`set(value, false)` and `Effect.timeout` are removed. Wire: none.
