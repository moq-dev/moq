# [L] A narrowing ends what it removes

## Goal

A narrowing that removes a path ends the broadcasts already handed out under
it, the group in flight included, instead of being refused so the relay closes
the whole session. A moderation decision (a deafened user's audio path) then
takes effect on the session it targets while its other subscriptions keep
flowing, which a forked client cannot bypass. This is the per-subscriber
exclusion [#2714](https://github.com/moq-dev/moq/issues/2714) asked for.

## Plan

Decide where the revocation is enforced before building it:

- **Model, per subscription.** A handed-out broadcast shares its state with the
  front every other session reads, so ending one session's copy needs state per
  handout reaching the track, subscription, and group readers. The cheapest
  shape found rides the existing group expiry check (`GroupExpiry` in
  `track.rs`), which runs only on a blocked read and already parks on the
  subscription's own state, so a private revoked signal there wakes parked
  readers for free. It still needs a registry of subscriptions per handout (a
  lock and a push per subscribe, on every relay session), an expiry reason so
  readers see `Unauthorized` rather than `Old`, and checks on ordered reads,
  datagrams, fetch, and peek. Decide whether that per-subscribe cost is
  acceptable.
- **Session.** The `moq-net` publisher watches one narrowing signal per session
  and resets the subscriptions outside the grant. Cost is per session, not per
  frame, but a transport reading the model directly is not bound by it.
- **Keep closing.** Leave `Unsupported` as the answer and close the session.

Also decide the publish side: whether a route or broadcast a session published
outside a narrowed grant is retracted, aborted, or still refuses the narrowing.

Benchmark any per-subscriber cost with `origin/narrow` in
`rs/moq-net/benches/origin.rs`, swept over routes and subscribers.

## Closes

- [#2714](https://github.com/moq-dev/moq/issues/2714) - close this issue when the quest finishes

## Related

- [Origin narrowing](/quest/m1/origin-narrowing.md) - the in-place narrowing this extends
- [In-band auth](/quest/m1/auth/README.md) - a shrinking token union cancels the work it no longer covers
