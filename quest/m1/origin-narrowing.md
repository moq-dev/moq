# [L] A live origin grant narrows in place

## Goal

A `moq-net` origin handle's grant can be replaced by a narrower pattern union
while the session is live: announce cursors retract the patterns that fell
outside, resolution refuses them, and broadcasts already handed out under
them end rather than draining. The relay uses it on revalidation so a
moderation decision (a deafened user's audio path) takes effect on the
session it targets, which a forked client cannot bypass. This is the
per-subscriber exclusion [#2714](https://github.com/moq-dev/moq/issues/2714)
asked for; the scope and match half already landed in #3672 and #3746.

A narrowing always succeeds. Until it can end what it removes, the relay keeps
closing the session on a narrowed grant: that is simple, secure, and correct.

## Plan

- Narrowing and handout revocation ship together. #3971 built the narrowing
  alone and was closed: it had to refuse whenever anything live sat in the
  removed part (the session's own reads, another session's reads of the same
  front, or its own publishes), which is exactly when a grant narrows, so it
  only saved a reconnect for access nobody was using.
- `origin::Producer` and `origin::Consumer` gain one narrowing operation on
  the handle, shared by every handle a session derived from it. A grant that
  is not a subset of the current one is refused; widening is never a
  narrowing. If handles need a tree to share it, keep the ceiling on the grant
  node itself (an `Arc` shared by clones and children), not in a side map on
  the origin.
- Revocation needs handouts attributed to the grant that requested them.
  Decide where it is enforced first:
  - **Model, per subscription.** A handout shares its state with the front
    every other session reads, so ending one session's copy needs state per
    handout reaching the track, subscription, and group readers. The cheapest
    shape found rides the group expiry check (`GroupExpiry` in
    `rs/moq-net/src/model/track.rs`), which runs only on a blocked read and
    parks on the subscription's own state, so a private revoked signal wakes
    parked readers for free. It still needs a registry per handout (a push per
    subscribe on every relay session), a reason so readers see `Unauthorized`
    rather than `Old`, and checks on ordered reads, datagrams, fetch, and peek.
    The group in flight ends too: a group may stay open as long as its track,
    so its boundary is no boundary at all. A transport that never learns about
    the change still cannot keep reading.
  - **Session.** The `moq-net` publisher watches one narrowing signal per
    session and resets the subscriptions outside the grant. The cost is per
    session, not per frame, but a transport reading the model directly is not
    bound by it.
- Decide the publish side: whether a route or broadcast the session published
  outside the narrowed grant is retracted or aborted.
- Relay revalidation uses it: a re-checked grant with the same root narrows
  the session's origin handles instead of closing the session
  (`Lease::ended` reporting "grant narrowed" in `rs/moq-relay/src/auth.rs`),
  on every accept path (native, WebSocket, io_uring, `moq --listen`). A
  changed root still closes.
- Prove the deafen case at the model layer: subscribe under a room prefix,
  narrow with a grant that excludes that audio path, and assert the existing
  subscription closes with `Unauthorized` and no further objects arrive,
  while a sibling path under the same prefix keeps flowing.
- Benchmark the per-subscribe and routing cost in
  `rs/moq-net/benches/origin.rs`, swept over routes and subscribers, with an
  origin that never narrows as the baseline.

Public API: additive on moq-net (one method) and on moq-relay's revalidate
path. Wire: none.

## Closes

- [#2714](https://github.com/moq-dev/moq/issues/2714) - close this issue when the quest finishes

## Related

- [In-band auth](/quest/m1/auth/README.md) - the token union that a narrowing later revalidates
