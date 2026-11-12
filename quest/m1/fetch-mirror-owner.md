# [S] A FETCH of a failed group keeps its frames reclaimable

## Goal

A JS FETCH caller holding a failed group (`fetchGroup` in `js/net/src/broadcast.ts`) keeps that group's mirror registered in its cache entry (`entry.mirrors` in `js/net/src/track.ts`) until the caller drains or releases it.
The unread frames it holds then stay billed to the pool and reachable by its eviction, like any other reader's.

## Plan

- Found in review of [moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476); `main` has the same shape. `fetchGroup` closes its subscription once the returned group closes, and closing the sink deletes its mirror from `entry.mirrors`. A group that fails closes at once, so the frames its caller has not read yet leave the entry's reach while the caller still holds them. This holds for any failed group, including a live group the publisher cuts.
- Tie the mirror's release to the caller's group consumer finishing (drained or closed by the caller), not to the group's own close.
- Test it on mock time: a pool under debt evicts the unread frames of a failed group a FETCH caller still holds.
