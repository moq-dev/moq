# [S] A live track lookup never wakes the broadcast

## Goal

Looking up a track that a broadcast already holds live takes a non-waking
read of the broadcast state, so it no longer wakes the relay front's driver
and every `broadcast::Demand` watcher on each lookup. Only a miss or a closed
entry takes the write path.

## Plan

Found by [Demand polls never lose a wake](/quest/m0/demand-lost-wake.md):
`track_inner` (`rs/moq-net/src/model/broadcast.rs`) locks the state for every
lookup because `WeakCache::get` takes `&mut` to drop a closed entry, and that
write wakes every watcher. That accidental wake hid the front's lost wake;
demand-lost-wake's front loom models now guard against it returning.

Decided 2026-10-08:

- Read first, write on a miss: under a read, return the entry only if
  `try_consume()` succeeds. A missing or closed entry falls through to
  today's write path, which re-checks everything, so there is no race
  between the read and the write. Rejected: making `get` leave closed
  entries for GC, which still takes the write lock and wakes on every
  lookup.
- Lands as hygiene regardless of the numbers. Add a bench swept over
  subscribers per track and tracks per broadcast that counts watcher wakes,
  to guard against regressions.

Public API: none. Wire: none.

## Required

- [Demand polls never lose a wake](/quest/m0/demand-lost-wake.md) - its front models catch a lost wake this exposes
