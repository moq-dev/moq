# [XS] js/net: a fractional maxAge is rounded up before it reaches the wire

## Goal

A `Subscription.maxAge` computed from a float, such as `1.25 x RTT`, opens a
subscription. Today on dev the varint encoder throws `RangeError` from
`BigInt` on any non-integer, both media subscriptions fail inside `spawn`
with no retry, and `<moq-watch>` in auto mode sits at `status=live` with zero
bytes on every real path.

## Plan

Branch from dev; main is not affected. main rounds in
`js/watch/src/audio/subscription.ts` (from #3114), a file dev deleted in the
#3396 refactor, and every merge of main into dev resolved the conflict by
dropping it. dev's `js/watch/src/media.ts` `subscribeMedia` passes
`sync.out.maxAge` unrounded to three callers (audio, video, text).

- Normalize in js/net, where `Subscription` documents `maxAge` as
  milliseconds: `Math.ceil` in `subscriptionDefaults` and
  `combineSubscriptions` (`js/net/src/track.ts`). Ceil, not round: it is a
  budget, and rounding down can drop a group. Every caller and every embedder
  building a `Subscription` is covered; `js/watch` needs no change.
- Regression test in `track.test.ts` feeding 38.75 and 500.25 through a
  subscribe and an update, asserting the encoded SUBSCRIBE carries 39 and
  501. Restore an equivalent of #3114's test so the next merge cannot drop
  the guard silently again.

## Closes

- [#3478](https://github.com/moq-dev/moq/issues/3478) - close this issue when the quest finishes

## Related

- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - the mode that produces the fractional value
