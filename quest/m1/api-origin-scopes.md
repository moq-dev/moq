# [M] Origin scopes

## Goal

A live `moq-net` origin grant can be narrowed in place, ending what it no
longer covers, and every binding that exposes announcements reports the
announce match the way Rust and JS do. The origin scope API already carries an
arbitrary pattern union with a literal root, and Rust and JS announce events
carry the match with its captures (PR for the first half of this quest).

## Plan

- Runtime narrowing. `origin::Producer` and `origin::Consumer` gain a way to
  replace a live handle's grant with a narrower union, shared by every handle
  a session derived from it: announce cursors retract the patterns that fell
  outside, resolution refuses them, and broadcasts already handed out under
  them end rather than draining. Decide how a handed-out broadcast ends (an
  abort with `Unauthorized` at the next group boundary is the simplest sound
  option) and whether the origin tracks hand-outs weakly per handle or the
  session ends its own subscriptions from a scope-change event.
- Relay revalidation uses it: a re-checked grant with the same root narrows the
  session's origin handles instead of closing the session (`Recheck::Closed
  ("grant narrowed")` in `rs/moq-relay/src/connection.rs`), so the lease the
  relay holds (`moq_auth::lease`) is the moderation boundary a forked client
  cannot bypass. A changed root still closes.
- Prove the deafen case at the model layer: subscribe under a room prefix,
  narrow with a grant that excludes that audio path, and assert the existing
  subscription closes and no further objects arrive.
- Mirror the announce match in the bindings. `moq-ffi`'s
  `MoqOriginConsumer::announced(prefix)` scopes by a literal root today, whose
  only wildcard is the implicit trailing `**`, so a capture there would equal
  the pattern. Take a pattern scope instead, with events relative to the origin
  and `captures` on `MoqAnnounceUpdate`, and follow the Cross-Package Sync
  checklist: `rs/libmoq` (`moq_announce_update`), the `py`, `swift`, `kt`,
  `dart`, and `go` wrappers, `doc/lib/*`, and `just test smoke-full`.

## Closes

- [#2714](https://github.com/moq-dev/moq/issues/2714) - close this issue when the quest finishes

## Related

- [Pattern interest](/quest/m2/path-patterns/interest.md) - carries the same scopes on the lite-06 wire once they are enforced here
