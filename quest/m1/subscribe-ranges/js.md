# [L] JS ranges

## Goal

`@moq/net` matches Rust: `Subscription` carries ranges beside its existing
`order`, the lite-07 SUBSCRIBE carries them on the wire, FETCH is gone from lite-07, and
`fetchGroup` becomes a one-range subscription or is removed. A JavaScript
publisher serves requested ranges that are no longer in its live cache.

## Plan

Mirror the Rust names, including `Time(pts)` ends and their refusal on an untimed track.

Decided in the 2026-10-05 audit: the producer half of
[JavaScript FETCH](/quest/m1/js-fetch.md) folds in here, so `@moq/net`'s
on-demand request surface takes range requests from the start instead of the
per-group `requested_group` shape Rust is replacing. That quest keeps only
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

When ranges add frame bounds, the group model carries a lite group's
`frameStart` instead of refusing a nonzero one, as
[partial lite groups](/quest/m1/release-audit/js-partial-group.md) does until
then. Preserve the per-reader group-bound rule for reliable reads, live-edge
selection, subscription updates, and publisher datagrams.

## Required

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the wire this speaks

## Related

- [Partial lite groups](/quest/m1/release-audit/js-partial-group.md) - refuses a nonzero `frameStart` until this models it
