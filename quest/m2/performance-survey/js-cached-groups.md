# [M] Joining a cached JS track avoids repeated full sorts

## Goal

A subscriber joining retained groups, or falling behind while groups arrive,
does no unnecessary whole-backlog sorting.

## Plan

[Track replay](/js/net/src/track.ts) mirrors every cached group on join
(line 1061); each mirror pushes and sorts the entire unread group array
(lines 1096-1098). Even ordered replay sorts arrays of sizes 1 through G.
The separate timeline index already has an append fast path and ordered
insertion. Existing track timing drains immediately and misses growing queues.

Benchmark retained groups × joining subscribers, then unread backlog × active
subscribers, in a real browser. Hold payload work fixed where possible and
include monotonic, reverse and mixed arrivals. Report comparator visits,
join/append latency, allocation/GC cost and event-loop delay. Prove replay
contents, ascending delivery, sparse arrivals, eviction, close and retained
mirror lifetimes remain identical. Reuse existing ordering machinery if it
makes the result simpler.

This is distinct from subscription update traffic in
[moq-dev/moq#6305](https://github.com/moq-dev/moq/pull/6305) and the storage
contract in [unbounded JS groups](/quest/m1/unbounded-groups/js.md).

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
