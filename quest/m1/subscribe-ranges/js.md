# [L] JS ranges

## Goal

`@moq/net` matches Rust: `Subscription` carries ranges beside its existing
`order`, the lite-07 SUBSCRIBE carries them on the wire, and `fetchGroup`
becomes a one-range subscription or is removed. A JavaScript publisher serves
requested ranges that are no longer in its live cache.

## Plan

Decided 2026-10-27 (maintainer): removing FETCH from lite-07 moved to
[Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md), and
this quest no longer waits on
[Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md). The rest starts now:
the ranges model, anchors, `maxDelay: null`, and the on-demand request
surface. Rust's lite-07 SUBSCRIBE already carries ranges
([moq-dev/moq#6715](https://github.com/moq-dev/moq/pull/6715)).

Mirror lite-07 bounded completion in the publisher: a covered bounded
subscription FINs without SUBSCRIBE_END. The JS subscriber already accepts that
FIN, closing the request's track once its tail settles, and failing it when an
update moved the range and the groups that arrived leave it uncovered. Reopen
that demand on the same track instead, as the Rust copy does.

Mirror the Rust names, including `Time(pts)` ends and their refusal on an untimed track.
The publisher resolves no relative or time start, serving it from the oldest
group its budget allows, and SUBSCRIBE_OK `Range Start` says so; resolve them.
Also mirror Rust's optional `Subscription::max_delay`
([moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171)): a `null`
`maxDelay` asks for the publisher's max age alone, dominates when subscriptions
combine, and goes out as the largest delay the wire allows, where today an
omitted one means the live edge only.

Fills mirror Rust's rule from [#7168](https://github.com/moq-dev/moq/pull/7168)
(decided 2026-10-29, maintainer): give up a run below a start already
resolved under a budget at least as loose as the reader's, and on a pre-06
wire fill below the start only when the reader names a floor. `js/net`
requests no fills today, so nothing over-fetches yet; this keeps it that way
once it does.

Decided in the 2026-10-05 audit: the producer half of
JavaScript FETCH folds in here, so `@moq/net`'s
on-demand request surface takes range requests from the start instead of the
per-group `requested_group` shape Rust replaced with `requested_fill`. IETF
FETCH dispatch onto it is
[on-demand JavaScript FETCH](/quest/m1/js-fetch-on-demand.md). Resized from
[M] to [L] for it.

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

Ranges stay whole groups ([moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465)),
so the group model keeps refusing a nonzero lite `frameStart` as a protocol
violation, as `lite/subscriber.ts` `runGroup` does. Preserve the per-reader
group-bound rule for reliable reads, live-edge selection, subscription
updates, and publisher datagrams.

## Related

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the Rust lite-07 range fill this mirrors
- [Remove FETCH from lite-07](/quest/m1/subscribe-ranges/lite07-fetch.md) - follows this and Lite-07 ranges
