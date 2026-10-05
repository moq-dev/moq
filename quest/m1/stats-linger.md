# [S] Stats broadcasts linger

## Goal

A grouped stats broadcast (`<prefix>/<group>/node/<node>`) stays announced for
a linger after its group's last session and traffic row leave, so a viewer
leaving and another arriving within it causes no unannounce and no new
announce across the mesh. The counters it reports, and any usage read from
them, are unchanged.

## Plan

Decided 2026-10-05 in moq.pro's quest audit: approved as recommended. The
earlier plan was in the closed
[#4052](https://github.com/moq-dev/moq/pull/4052) beside tree-routed
announces; the linger is independent of that and lands alone.

Data from moq.pro's live fleet: on 2026-09-29 one customer's node stats
broadcasts ended on 31 nodes at once every 2 to 4 minutes and returned 46 to
112 s later, and the fleet served about 148,000 `.stats` subscriptions against
7 media ones in that hour. Size the linger from those gaps, not the one minute
first proposed, and make it configurable.

- `moq-stats`'s producer unpublishes a group broadcast on the drain where its
  group has no traffic or session rows (`publish` in
  `rs/moq-stats/src/produce.rs`). Keep it, and the epoch and group sequence it
  publishes under ([stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md)), until the
  linger elapses with the group still empty; a row returning re-arms it.
  While it lingers empty, its live gauges and session presence read zero and
  its cumulative traffic totals are kept, so a reader sees no stale live
  counters.
- Time decisions use `max(wall, pts)`; tests mock time.
- Depth 0 already lives for the producer's life and is unchanged.

moq.pro tracks the `release` branch, so once this lands on `main` it is
backported to `release` (a cherry-pick PR).

Test: a session closes and reopens within the linger with no unannounce;
while the group lingers empty, live gauges and presence are zero and
cumulative totals are unchanged; the group unannounces after the linger; the
reported totals match a run without the linger.

Public API: a linger knob on the stats producer config. Wire: none.

## Related

- [Stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md) - the name a lingering broadcast keeps
- [Bounded stats aggregate](/quest/m0/broadcast-epoch/stats-aggregate-bound.md) - the aggregator's grace window for departed nodes
- [moq.pro: stats linger](https://github.com/moq-dev/moq.pro/blob/main/quest/m0/stats-linger.md) - the fleet adoption and its measurements
