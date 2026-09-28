# [S] Watch shows a refusal

## Goal

When the origin refuses the broadcast `<moq-watch>` asks for, the player shows
that refusal as an error instead of sitting offline as if nothing was
published yet. Refusal stays terminal, as
[#4230](https://github.com/moq-dev/moq/pull/4230) made it in `@moq/net` to
match Rust moq-net: the player never re-asks a handler that already said no.

## Plan

- The gap is Codex's P1 on #4230
  ([r4109909244](https://github.com/moq-dev/moq/pull/4230#discussion_r4109909244)):
  `js/watch/src/broadcast.ts` only watches `request.active`, so after a
  `dynamic()` handler refuses, the request closes with an error that nobody
  reads and `active` stays `undefined` forever.
- Observe `Requesting.closed` and carry the error into the broadcast's
  state. Whether that is a new `"error"` status, a separate error signal, or
  both is open; mirror how the element already surfaces other terminal
  states, such as the unsupported indicator. Keep the error's message so the
  UI can say why. `unroutable` is also true for a path nothing serves yet, so
  it cannot tell a refusal from offline.
- What clears the error is part of the design: a fresh request (a new
  `name` or origin, or re-enabling) should be the only way back. No retry
  loop.
- Cover both the announced and unannounced paths in `#runBroadcast`.
- Show it in the UI, and update `demo/web` if it consumes the status. Add a
  test in `js/watch` where a `dynamic()` handler refuses and the broadcast
  reports the error.
- Update `doc/` wherever the watch status values are documented.

Public API: likely additive (a new status value or error signal on the watch
broadcast and element). Wire: none.

## Related

- [#4230](https://github.com/moq-dev/moq/pull/4230) - made JS refusals terminal
