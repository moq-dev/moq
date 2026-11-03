# [M] Plan relay-per-thread on a worker mesh that never re-announces

## Goal

A recorded plan, with the maintainer's decisions, for running the relay as
one cluster per worker thread linked by a mesh that never re-announces peer
routes to peers. It weighs that design against the replica spike's
per-track replicas, sets how routes from outside the relay reach every
worker, measures announce state and propagation with no-re-announce, and
decides whether [Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md)
waits on it.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2, in this epic

From the epic's 2026-11-01 cluster-spike verdict
([moq-dev/moq#7534](https://github.com/moq-dev/moq/pull/7534)): a conditional
win, planned on the condition that the worker mesh never re-announces peer
routes. The epic README holds both spikes' numbers.

- Measure the no-re-announce mesh: heap and allocations per broadcast and
  announce delay at 1 to 16 workers (the cluster spike's `spike_announce`
  table), plus the data-plane shapes (chat, fanout, video-heavy).
- Settle how other relays' routes reach every worker without re-announce,
  for example through [Routes and announces](/quest/m1/cluster-routing/routes.md)'
  per-connection `proxy` preference turned off on worker links.
- Compare against the replica spike
  ([moq-dev/moq#7533](https://github.com/moq-dev/moq/pull/7533)): replicate
  per track on demand above a fan-out threshold, with one shared announce
  table.
- Bring the choice, and whether Log adoption waits on it, back to the
  maintainer; the result becomes implementation quests through
  `/quest-plan`.

Public API: none (a plan). Wire: none.

## Related

- [Cluster route memory](/quest/m2/perf/cluster-route-memory.md) - shrinks the per-route state a full mesh multiplies
- [Shard by default](/quest/m2/perf/shard-default.md) - the thread-per-core workers this runs on
