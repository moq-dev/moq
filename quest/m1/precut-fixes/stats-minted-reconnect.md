# [S] The stats aggregator counts a reconnected node once

## Goal

`moq-stats`'s aggregator keeps one entry per node across a reconnect on a
route whose instance a session minted (every version before lite-07, and
moq-transport), so a cluster link redial, a route failover, or a relay
restart within the aggregator's grace (`Config::grace`, 10 minutes) never adds a node's cumulative counters on top of its own retained
contribution. A real node restart is still detected by its counters going
down, as `doc/concept/stats.md` says for minted routes.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; confirmed from source.
`Merged::apply_announce` (`rs/moq-stats/src/aggregate.rs`) keys nodes by
`(path, instance)`. Below lite-07 the receiving session mints a fresh
instance per session for each advertised path (`Minted::acquire` in
`rs/moq-net/src/lite/subscriber.rs`), and lite-06 is the default version.
After a redial the same node arrives under a new key, its cumulative totals
count from the start again, and once the old entry's 10-minute grace ends
its last frame folds into `retired`: the node is counted twice for good.
`release` keyed nodes by path only, so it does not have this.

Recommended: when the new instance `is_minted()` and the path's previous
entry is also minted, carry the previous entry over to the new key, and let
the existing `regressed` check treat a counter going down as a restart.
A lite-07 instance a publisher named keeps today's per-instance entry.
A return after the grace starts over from zero, as `Merged`'s docs already
say for every route: the old entry has folded into `retired` and its path
lookup is gone, and bounding a per-node ledger of retired nodes would cost
memory for every departed node. This quest keeps that documented limit.

Test: a simulated-clock aggregator test that unannounces and re-announces a
node path under a fresh minted instance, with counters continuing, and
asserts the merged total after the grace equals the node's last frame; and
the boundary, a re-announce just after the grace, which starts over as
documented.

Public API: none. Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#7489](https://github.com/moq-dev/moq/pull/7489) - keyed aggregator nodes by announced instance
