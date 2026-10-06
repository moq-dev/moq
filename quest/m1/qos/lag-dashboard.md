# [S] Demo dashboard shows viewer lag

## Goal

The demo stats dashboard (`demo/web/src/stats.ts`) shows the relay's viewer
lag and dropped media: percentiles of the egress `lag` histogram over the
chart window, and `dropped` duration, bytes, and groups as rates, cluster-wide
and per node like the existing counters.

## Plan

- Read both from [stats totals and prefix
  tracks](/quest/m0/broadcast-epoch/stats-split.md), which lands first and
  retires the `publisher.json` rows they sit on today (decided in the
  2026-10-06 audit); #4133 rebases them onto the egress side of its per-group
  totals and prefix tracks. `lag` is a cumulative byte count per bucket keyed
  by its upper edge (`"50ms"` to `"5s"`, then `"inf"`, empty buckets
  omitted); `dropped` is `{ duration, bytes, groups }` with the duration in
  fractional milliseconds. `doc/concept/stats.md` documents them. Diff two
  samples within one subscription for an interval's distribution, as the
  dashboard already does to turn cumulative bytes into rates, and sum nodes
  bucket by bucket.
- A percentile read from buckets is a bucket edge, not a point value, and
  `inf` has no upper edge. Show it as "under X" or interpolate inside the
  bucket, and say which.
- Skip `.`-prefixed system broadcasts, as the existing aggregate does. Lag is
  per broadcast, so a per-broadcast view reads that broadcast's prefix track
  if it stays cheap.
- Extend the dashboard's existing relay-stats interfaces; no `@moq/stats`
  package is planned.

Public API: none. Wire: none.

## Required

- [Stats totals and prefix tracks](/quest/m0/broadcast-epoch/stats-split.md) - the totals and prefix tracks the dashboard reads lag from
