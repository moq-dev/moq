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
and time anchors per reader), `maxDelay: null`, the lite-07 publisher FINning a
covered bounded subscription without SUBSCRIBE_END, and the subscriber opening
a fresh SUBSCRIBE on the same track when that FIN leaves the demand uncovered.

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
- **Fills.** Mirror Rust's rule from [#7168](https://github.com/moq-dev/moq/pull/7168)
  (decided 2026-10-29, maintainer): give up a run below a start already
  resolved under a budget at least as loose as the reader's, and on a pre-06
  wire fill below the start only when the reader names a floor. `js/net`
  requests no fills today, so nothing over-fetches yet; this keeps it that way
  once it does.

Ranges stay whole groups ([moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465)),
so the group model keeps refusing a nonzero lite `frameStart` as a protocol
violation, as `lite/subscriber.ts` `runGroup` does. Preserve the per-reader
group-bound rule for reliable reads, live-edge selection, subscription
updates, and publisher datagrams.

## Related

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the Rust lite-07 range fill this mirrors
- [Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md) - follows this and Lite-07 ranges
