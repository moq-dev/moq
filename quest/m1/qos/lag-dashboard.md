# [S] Demo dashboard shows viewer lag

## Goal

The demo stats dashboard (`demo/web/src/stats.ts`) shows the relay's viewer
lag and dropped media: percentiles of the egress `lag` histogram over the
chart window, and `dropped` duration, bytes, and groups as rates, cluster-wide
and per node like the existing counters.

## Plan

- The relay writes both on `publisher.json` rows only. `lag` is a cumulative
  byte count per bucket keyed by its upper edge (`"50ms"` to `"5s"`, then
  `"inf"`, empty buckets omitted); `dropped` is `{ duration, bytes, groups }`
  with the duration in fractional milliseconds. `doc/concept/stats.md` on the
  QoS line documents them. Diff two samples for an interval's distribution,
  as the dashboard already does to turn cumulative bytes into rates, and sum
  nodes bucket by bucket.
- A percentile read from buckets is a bucket edge, not a point value, and
  `inf` has no upper edge. Show it as "under X" or interpolate inside the
  bucket, and say which.
- Skip `.`-prefixed system broadcasts, as the existing aggregate does. Lag is
  per broadcast, so a per-broadcast view is worth adding if it stays cheap.
- The [browser stats quest](/quest/m1/qos/stats/js.md) moves the dashboard
  onto `@moq/stats` on the same line. If it has landed, read `lag` and
  `dropped` through its schemas; otherwise extend the existing interfaces and
  let whichever lands second reconcile.

Public API: none. Wire: none.
