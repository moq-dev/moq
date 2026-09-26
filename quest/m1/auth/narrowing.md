# [M] A live origin grant narrows in place

## Goal

A `moq-net` session's grant can be replaced by a narrower pattern union
while the session is live: announce cursors retract the patterns that fell
outside, resolution refuses them, subscriptions under them reset with
`UNAUTHORIZED`, and broadcasts the session published under them abort. The
relay uses it on revalidation so a moderation decision (a deafened user's
audio path) takes effect on the session it targets, which a forked client
cannot bypass. This is the per-subscriber exclusion
[#2714](https://github.com/moq-dev/moq/issues/2714) asked for; the scope and
match half already landed in #3672 and #3746.

A narrowing always succeeds. A changed root still closes the session.

## Plan

- Enforce per session, not per handout. The line's session gate for received
  grants (`auth::Gate`, the lite publisher resetting subscriptions it denies,
  `regrant` retracting announces no longer covered, and the IETF equivalent)
  is the same mechanism pointed at the relay's own grant. Reuse it rather than
  building a second one. Readers that bypass the session publisher (HTTP
  `/fetch` holds its own lease and closes on it; moq-hls and WHEP enforce no
  per-user grant today) are out of scope.
- #3971 built narrowing alone and was closed: it refused whenever anything
  live sat in the removed part, which is exactly when a grant narrows, and it
  kept the ceiling in a side map. Scopes today are copied by value into every
  handle and cursor (`OriginScope` in `rs/moq-net/src/model/origin.rs`), so
  narrowing needs shared state. If handles need a tree to share it, keep the
  ceiling on the grant node itself (an `Arc` shared by clones and children).
- Publish side: routes and broadcasts the session published outside the
  narrowed grant abort, so consumers see `Unauthorized` just as the subscribe
  side does. Draining is not an option: a group may stay open as long as its
  track.
- Relay revalidation uses it: a re-checked grant with the same root narrows
  the session instead of closing it (`Lease::ended` reporting
  `Reason::Narrowed` in `rs/moq-relay/src/auth.rs`), on every accept path: native,
  io_uring, and `moq --listen` share `connection::supervise`, while WebSocket
  has its own copy of the loop.
- Prove the deafen case end to end: subscribe under a room prefix, narrow
  with a grant that excludes that audio path, and assert the subscription
  resets with `UNAUTHORIZED` and no further objects arrive, while a sibling
  path under the same prefix keeps flowing.
- Benchmark the narrowing and per-session gate cost in
  `rs/moq-net/benches/origin.rs`, swept over routes and subscribers, with a
  session that never narrows as the baseline.

Public API: additive on moq-net and on moq-relay's revalidate path. Wire:
none beyond the UNAUTHORIZED code the required quest adds.

## Required

- [Unauthorized reset](/quest/m1/auth/unauthorized.md) - the stream code a narrowed subscription resets with

## Closes

- [#2714](https://github.com/moq-dev/moq/issues/2714) - close this issue when the quest finishes
