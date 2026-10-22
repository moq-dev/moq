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

The 2026-10-22 release audit reproduced a P2 defect in already accepted
datagram filters at main 1d43c096e0. For an inclusive request from
(group 0, object 1) through (group 1, object 0), publishing datagrams at
(0, 0), (1, 0), and (2, 0) should deliver only (1, 0). Each datagram is object
0 of its group; the observed lists below contain group sequences.
The real JS publisher with paired
mock transports delivered [0, 1, 2] on IETF drafts 20/22 and [0, 1] on Lite
06/07. IETF's datagram task receives no resolved range, losing both bounds;
Lite installs the group cap but does not pass frame bounds to its datagram
task. The three existing reliable-group range controls passed. These were
local publisher/codec reproductions; the native counterpart was inspected
but not exercised by these targeted tests.

Keep this correction under the existing range work, including currently
published Lite-06 behavior and subscription updates. Lite's defect is
inherited; JS IETF datagrams newly expose it after
[moq-dev/moq#4979](https://github.com/moq-dev/moq/pull/4979).
Correcting an accepted filter needs no additional API or wire format by
itself. Test datagrams against both object boundaries as well as group/time
ranges, so the reliable and unreliable paths share the same selection rule.

## Required

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the wire this speaks
