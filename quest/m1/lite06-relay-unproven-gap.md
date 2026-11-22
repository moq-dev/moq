# [M] A lite-06 relay asked about a gap it cannot prove asks upstream

## Goal

A lite-06 relay copy serving a SUBSCRIBE that starts inside a run of keys it holds no answer
for (no group, no drop, gap not folded) asks upstream about that run and passes the answer on.
This is the cache rule in `AGENTS.md`: a relay that can't prove an answer must ask upstream and
wait for its answer. Today the relay neither proves nor asks, so a reader two relays from the
publisher with no max delay waits until the track ends. The gap predates
[moq-dev/moq#9377](https://github.com/moq-dev/moq/pull/9377): before it, the downstream copy walked
the gap one FETCH per key instead (61,000 FETCHes in 600 s of simulated time, about 8 hours for
3 s of media, in the test below).

## Plan

`lite06_fill_walk::a_relay_start_naming_no_group_costs_no_subscribe_per_key` has the scenario:
publisher → relay A (keeps every group) → relay B (expires them). A reader on B asks from key 1
with no max delay. B recovers each run between groups on a SUBSCRIBE of its own. A answers
SUBSCRIBE_START at the run's first key (its source's start raised to the floor), then serves
nothing in the run and never drops it: its live tail never folds the run (the grace is the max
delay, here none), and its publishing cursor goes by arrival order, so the run is no miss that
would ask upstream. The test ends the track to settle the runs (`drop_unserved`).
Without that, the reader waits until the track ends. Upstream would answer at once: the
publisher's START names the run's next group.

Recommended: when a lite-06 `TrackRun` resolves its start below its first held group and the
copy can't prove the keys between, the copy asks upstream for that run, as a reader's miss
does (a per-run recovery), and drops what upstream gives up. Start with the test above, minus
the `track.finish()`. Check lite-07 too, where a range's start resolves the same way, and the
JS publisher.
