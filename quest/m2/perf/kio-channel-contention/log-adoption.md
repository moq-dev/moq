# [M] Log adoption across moq-net

## Goal

Every moq-net channel whose state is an append log with front eviction moves
onto the kio append-only log (`kio::log`),
where it measures as a win. Each migrated channel lands with the contended
bench or `just bench-profile lock` before and after; a channel that measures no win
stays on its mutex.

## Plan

- `TrackState` arrival order first: the baseline's hottest channel, moved
  here from the log quest on 2026-10-24 when that quest narrowed to
  `GroupState`'s frames so Front eviction could land on it in m1. It pops its
  front; its demote/evict writes and the scalar fields beside the log stay on
  the existing state.
- Waits on the spikes (decided 2026-10-14 for the log, kept here): with
  per-thread sessions every consumer of a replica is on one thread, so its
  mutex is uncontended and a lock-free multi-reader log only pays off if the
  shared model survives.
- Audit moq-net (and moq-relay's `critical.rs`) for other state that is
  appended at the back, read by cursor, and evicted from the front, beyond
  `GroupState` frames, which the log quest already moves. List the
  candidates with their hot path and fan-out in the PR before migrating.
- Maps and arbitrarily mutated state (`OriginState` routes, `BroadcastState`
  requests) are not log-shaped and stay put.
- Split the PR per channel if the audit finds more than a couple.

## Required

- [Replica spike](/quest/m2/perf/kio-channel-contention/spike-replica.md) - if per-worker replicas win, each channel's cross-core readers collapse to one flow-controlled stream per worker and adoption is likely abandoned
- [Cluster spike](/quest/m2/perf/kio-channel-contention/spike-cluster.md) - the same question with nothing shared
