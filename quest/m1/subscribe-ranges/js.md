# [L] JS ranges

## Goal

`@moq/net` matches Rust: `Subscription` carries ranges beside its existing
`order`, the lite-07 SUBSCRIBE carries them on the wire, and `fetchGroup`
becomes a one-range subscription or is removed. FETCH is gone from lite-07 in
both Rust and JS. A JavaScript publisher serves requested ranges that are no
longer in its live cache.

## Plan

Decided 2026-10-25 (maintainer, on
[moq-dev/moq#6807](https://github.com/moq-dev/moq/pull/6807)): the coordinated
removal of FETCH from lite-07 lands here, in Rust and JS together, once
[Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) fills through the
subscription. Rust stops sending and serving lite-07 FETCH, and
`drafts/draft-lcurley-moq-lite.md` drops FETCH from lite-07 with its changelog
bullets (FETCH max age and epoch). Published versions (lite-03..06) keep FETCH.
Mirror lite-07 bounded completion in the publisher too: a covered bounded
subscription FINs without SUBSCRIBE_END. The JS subscriber already accepts that
FIN, closing the request's track once its tail settles, and failing it when an
update moved the range and the groups that arrived leave it uncovered. Reopen
that demand on the same track instead, as the Rust copy does.

Mirror the Rust names, including `Time(pts)` ends and their refusal on an untimed track.
Also mirror Rust's optional `Subscription::max_delay`
([moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171)): a `null`
`maxDelay` asks for the publisher's max age alone, dominates when subscriptions
combine, and goes out as the largest delay the wire allows, where today an
omitted one means the live edge only.

Decided in the 2026-10-05 audit: the producer half of
[JavaScript FETCH](/quest/m1/js-fetch.md) folds in here, so `@moq/net`'s
on-demand request surface takes range requests from the start instead of the
per-group `requested_group` shape Rust replaced with `requested_fill`. That quest keeps only
IETF FETCH dispatch. Resized from [M] to [L] for it.

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

## Required

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the wire this speaks
