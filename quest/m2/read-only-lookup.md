# [S] A live track lookup never wakes the broadcast

## Goal

Looking up a track that a broadcast already holds live never mutates the
broadcast state, so it no longer wakes the relay front's driver and every
`broadcast::Demand` watcher on each lookup. Only a miss or a closed entry
mutates.

## Plan

Found by #5091: `track_inner` (`rs/moq-net/src/model/broadcast.rs`) mutates
the state on every lookup because `WeakCache::get` takes `&mut` to drop a
closed entry, and that mutation wakes every watcher. That accidental wake hid
the front's lost wake; the front loom models in `rs/moq-net/tests/loom.rs`
now guard against it returning.

Decided 2026-10-08:

- Probe first, mutate on a miss. A `kio` guard wakes watchers only once it is
  dereferenced mutably, and `read()` and `lock()` share one mutex, so the
  probe can run on the same guard the miss path uses: no reacquire or
  re-check. Under that guard, still refuse a closing broadcast with
  `Error::Unroutable` first (`try_consume()` alone succeeds on a track that
  `close()` spared), then return the entry only if `try_consume()` succeeds.
  `WeakCache` needs a non-mutating probe for this, since `get` drops closed
  entries. A missing or closed entry falls through to today's mutating path.
  Rejected: making `get` leave closed entries for GC, which keeps dead
  entries around for no speed gain.
- Lands as hygiene regardless of the numbers. Add a unit test that a live
  hit wakes no `Demand` watcher, keep `close_spares_a_served_track` passing,
  and add a bench swept over subscribers per track and tracks per broadcast
  that counts watcher wakes.

Public API: none. Wire: none.
