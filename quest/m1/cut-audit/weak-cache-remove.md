# [XS] Removing a stale front is O(1) under the origin lock

## Goal

A restart wave over F paths (an upstream relay reconnecting with a new
epoch) costs O(F) total work under the origin lock, not O(F²). A benchmark
swept over fronts and restarting paths shows a flat per-request cost, and
front wakes happen after the origin's locks drop.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited.

`Consumer::request` (`rs/moq-net/src/model/origin.rs:6491`) calls
`state.fronts.remove(&key)` whenever a live front resolved on a non-current
instance, which is the first request on every path after an epoch change.
`WeakCache::remove` (`model/weak_cache.rs:121`) runs `ring.retain`, a scan of
every front holding clones of live handles, while the global origin lock is
held. `get` and `remove_if` are already O(1), with gc bounded by
`GC_PROBE`.

- Drop the scan: let the bounded gc retire the stale ring slot, or let the
  following `insert` supersede it. Keep the guarantee that a removed entry's
  handle is not pinned indefinitely.
- Extend `benches/origin.rs` with a sweep over fronts times restarting paths.
- While there: `AnnounceProducer::retract` (`origin.rs` about line 2793) and
  `DriverState::teardown` (about line 2944) resolve front requests while
  holding the table and server locks; collect them and resolve after the
  guards drop, as `update_if` already does.

Public API: none (`WeakCache` is crate-internal). Wire: none.
