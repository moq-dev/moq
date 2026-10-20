# [M] Log adoption across moq-net

## Goal

Every moq-net channel whose state is an append log with front eviction moves
onto the [kio append-only log](/quest/m2/perf/kio-channel-contention/log.md),
where it measures as a win. Each migrated channel lands with the contended
bench or `just bench-profile lock` before and after; a channel that measures no win
stays on its mutex.

## Plan

- Audit moq-net (and moq-relay's `critical.rs`) for state that is appended at
  the back, read by cursor, and evicted from the front, beyond `GroupState`
  frames and `TrackState` arrival order, which the log quest already moves.
  List the candidates with their hot path and fan-out in the PR before
  migrating.
- Maps and arbitrarily mutated state (`OriginState` routes, `BroadcastState`
  requests) are not log-shaped and stay put.
- Split the PR per channel if the audit finds more than a couple.

## Required

- [Append-only log](/quest/m2/perf/kio-channel-contention/log.md) - the primitive this adopts
