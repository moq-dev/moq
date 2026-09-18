# [M] A live origin grant narrows in place

## Goal

A `moq-net` origin handle's grant can be replaced by a narrower pattern union
while the session is live: announce cursors retract the patterns that fell
outside, resolution refuses them, and broadcasts already handed out under
them end rather than draining. The relay uses it on revalidation so a
moderation decision (a deafened user's audio path) takes effect on the
session it targets, which a forked client cannot bypass. This is the
per-subscriber exclusion [#2714](https://github.com/moq-dev/moq/issues/2714)
asked for; the scope and match half already landed in #3672 and #3746.

## Plan

- `origin::Producer` and `origin::Consumer` gain one narrowing operation on
  the handle, shared by every handle a session derived from it. A grant that
  is not a subset of the current one is refused; widening is never a
  narrowing.
- The origin tracks the broadcasts each scoped handle handed out, weakly, and
  narrowing aborts those outside the new grant with `Unauthorized` at once,
  the group in flight included: a group is a live stream that may stay open
  for as long as the track does, so waiting for its boundary is no boundary
  at all. Enforcement stays in the model so the proof needs no session, and a
  transport that never learns about the change still cannot keep reading.
- Relay revalidation uses it: a re-checked grant with the same root narrows
  the session's origin handles instead of closing the session
  (`Recheck::Closed("grant narrowed")` in `rs/moq-relay/src/connection.rs`),
  so the lease the relay holds (`moq_auth::lease`) is the boundary. A changed
  root still closes.
- Prove the deafen case at the model layer: subscribe under a room prefix,
  narrow with a grant that excludes that audio path, and assert the existing
  subscription closes with `Unauthorized` and no further objects arrive,
  while a sibling path under the same prefix keeps flowing.

Public API: additive on moq-net (one method) and on moq-relay's revalidate
path; starts on main after the merge. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on the pattern-union scope API that only dev has

## Closes

- [#2714](https://github.com/moq-dev/moq/issues/2714) - close this issue when the quest finishes

## Related

- [Bindings announce match](/quest/m1/api-origin-scopes.md) - the binding half of the same line
- [In-band auth](/quest/m2/auth/README.md) - the token union that a narrowing later revalidates
