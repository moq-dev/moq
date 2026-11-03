# [M] JS ranges

## Goal

`@moq/net` matches Rust: `fetchGroup` becomes a one-range subscription or is
removed, and a JavaScript publisher serves requested ranges that are no longer
in its live cache.

## Plan

Landed in [moq-dev/moq#6949](https://github.com/moq-dev/moq/pull/6949):
`Subscription.ranges` of `absolute`, `relative`, and `time` anchors in place of
`groups`, a per-reader window (`js/net/src/window.ts`) with the Rust time-end
walk, the union aggregate, lite-07 SUBSCRIBE carrying the list with times in
the track's timescale (older wires the covering span, refusing other relative
and time anchors per reader), the lite-07 publisher resolving relative and
time anchors and naming each range's start in SUBSCRIBE_START, FINning a
covered bounded subscription without SUBSCRIBE_END, and the subscriber
keeping that copy and opening a fresh SUBSCRIBE on the same track once the
demand moves past what it covered. `delay: { max: null }` landed with
[moq-dev/moq#7597](https://github.com/moq-dev/moq/pull/7597).

Reader completion, decided 2026-11-02 (maintainer, on
[moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545) and
[moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)), replacing the
2026-10-31 decision that a range-bounded reader parks:

1. Park in both: keep the copy on a covered FIN, and leave a read that should
   end to the request surface and `fetchGroup`.
2. ✅ Complete in both: a bounded read ends once every group in its ranges is
   handed out, dropped, skipped as stale, or given up, and only wire
   publishers park.
3. Leave it: JS copies keep releasing on a covered FIN.

JavaScript matches Rust: a plain reader judges its own window, as the Rust
`Cursor::is_exhausted` does, and a reader following route copies judges it on
the serving copy, as the Rust `Reader::exhausted` does. Wire publishers and
route copies read through the package-internal `hooks.parked`, the Rust
`track::Subscriber::parked`. A lite-07 copy still outlives a covered FIN, so
the cache answers the same range and a widening update still delivers.

Decided 2026-11-03 (maintainer): main moved the cursor paths under
[moq-dev/moq#6949](https://github.com/moq-dev/moq/pull/6949)
([moq-dev/moq#7486](https://github.com/moq-dev/moq/pull/7486),
[moq-dev/moq#7542](https://github.com/moq-dev/moq/pull/7542),
[moq-dev/moq#7597](https://github.com/moq-dev/moq/pull/7597),
[moq-dev/moq#7609](https://github.com/moq-dev/moq/pull/7609)), so the PR
re-ports its ranges onto them:

1. ✅ Re-port in place, on the same PR.
2. A fresh branch, closing and deleting this one.
3. Park it.

Mirror lite-07 bounded completion as the Rust copy implements it. The target
is [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md)' "Ends are signaled"
design: a covered bounded subscription sends SUBSCRIBE_FINISH, keeps the
Subscribe Stream open, and never FINs it. Until SUBSCRIBE_FINISH ships, the
wire FINs a covered bounded subscription without SUBSCRIBE_END, and the JS
subscriber reopens demand it no longer covers on the same track, as the Rust
copy does.

What remains:

- **On-demand request surface.** Decided in the 2026-10-05 audit: the producer
  half of JavaScript FETCH folds in here, so
  `@moq/net`'s on-demand request surface takes range requests from the start
  instead of the per-group `requested_group` shape Rust replaced with
  `requested_fill`. IETF FETCH dispatch onto it is
  [on-demand JavaScript FETCH](/quest/m1/js-fetch-on-demand.md).
  - Expose an owned request handle rather than a storage callback; dropping a
    request refuses it rather than leaving the subscriber waiting. Preserve
    group sequence, frame boundaries, payload bytes, and track properties.
    Storage, retention, and media catalog interpretation stay outside `js/net`.
  - One logical dynamic track per broadcast and name: a cache-miss request
    never creates a duplicate live producer.
  - Verify with an in-memory responder, without OPFS or an archive writer: a
    browser publisher serves a native subscriber after a group is evicted or
    was never cached, covering exact replay, empty and missing groups,
    concurrent requests, and cancellation while awaiting a reply.
- **`fetchGroup`.** Becomes a one-range subscription on the request surface
  above, or is removed.
- **Fills.** Received copies asking upstream for their misses, with Rust's
  below-start rule and head fill, moved to
  [JS received copies fill what they missed](/quest/m1/subscribe-ranges/js-fills.md)
  (maintainer, 2026-11-03). Until it lands, a JS reader counts a hole the
  feed no longer owes as given up, with no fill asked.

Ranges stay whole groups ([moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465)),
so the group model keeps refusing a nonzero lite `frameStart` as a protocol
violation, as `lite/subscriber.ts` `runGroup` does. Preserve the per-reader
group-bound rule for reliable reads, live-edge selection, subscription
and subscription updates. Datagrams move to their own track kind ([Datagram tracks](/quest/m1/datagram-tracks/README.md)).

## Related

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the Rust lite-07 range fill this mirrors
- [Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md) - follows this and Lite-07 ranges
