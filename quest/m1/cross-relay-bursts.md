# [L] Cross-relay delivery under bursts

## Goal

Bursty small-group tracks cross two relays without losing groups or stalling.
On cdn.moq.pro, publishers and a subscriber on different nodes saw groups
never arrive (FETCH for them unanswered for 2 s while the publisher stayed
connected), a subscription opened early deliver only a burst's tail, and
return tracks stall up to 31 s with `Stream(Old)`. The same workload on one
node, or one self-hosted relay, loses nothing.

## Plan

Reproduce first, on a local two-relay cluster with the reporter's harness
(offered in the issue) and the relay build cdn.moq.pro ran. Then fix what the
repro shows. Suspects: `Old` expiry or newest-first dropping on the relay hop,
and FETCH not forwarded or answered upstream. Re-measure the stalls after
[FIN wait expiry](/quest/m1/fin-wait-expiry.md), a candidate cause. Keep the
repro as a regression test in the cluster tests.

The issue's stale and flapping announcements after a clean close are
cluster-routing evidence, not this quest's scope.

## Required

- [FIN wait expiry](/quest/m1/fin-wait-expiry.md) - removes one candidate cause of the stalls before measuring

## Closes

- [#4349](https://github.com/moq-dev/moq/issues/4349) - close this issue when the quest finishes

## Related

- [Cluster routing](/quest/m1/cluster-routing.md) - owns the stale and flapping announcements from the same report
