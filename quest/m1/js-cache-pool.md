# [L] JS cache pool

## Goal

`js/net` bounds what a broadcast caches in bytes, as Rust does: a port of
Rust's per-broadcast `cache::Pool` (`rs/moq-net/src/model/cache.rs`) bills
every cached group's frames and fixed bookkeeping, and the drop set's track
and cursor runs at a fixed cost per run, and evicts under it the way Rust
does. Rust and JS mirror each other under Rust's default budget, with no new
setting.

## Plan

Decided 2026-11-03 (maintainer, /quest-unblock, recorded in
[Drop set](/quest/m1/drop-set.md)): port Rust's per-broadcast `cache::Pool`
to JS. Shared byte accounting per broadcast bills frames and drop-set runs (a
fixed cost per run), under Rust's default budget, with eviction and the
fold-into-floor or `TooFarBehind` behavior the drop set plans. Not chosen: a
per-track run cap; upstream backpressure. Split out of the drop set on
2026-11-03, since the pool lands on its own: its first consumer is the group
cache, and the drop set's run billing follows on top of it.

What JS has today: `js/net/src/group.ts` caps each group's frame tail at
`TAIL_BYTES` (32 MiB) and evicts that group's oldest frames past it, while
the track cache (`#prune` in `js/net/src/track.ts`) is purely time-based on
`CACHE_EXPIRY` (30 s). Nothing bills bytes across groups or tracks, and
`TrackState.drops` and each cursor's runs grow without bound under a peer
that fragments them.

Port, mirroring Rust's names and shape:

- A pool per broadcast, defaulting as `broadcast::Info::default` does in
  Rust: no byte capacity and `DEFAULT_EXPIRY` (30 s) idle expiry.
- A per-track account and a per-group charge billing payload plus a fixed
  per-group overhead, and a fixed per-run cost for drop-set runs.
- Write-time eviction debt: a track over capacity pays by evicting its own
  oldest groups with EVICTED, the latest live group protected, and the
  access-time mean ordering eviction across tracks.
- The idle-expiry sweep on the pool's clock, replacing `#prune`'s own timer.

Test on mock time: a capacity bounds cached groups across tracks of one
broadcast, evicting the oldest first and sparing a fresher track; idle groups
expire; a drop-only flood with no cached groups stays within the pool once
[Drop set](/quest/m1/drop-set.md) bills its runs; the per-group frame-tail
limit still applies.

Public API: none beyond what Rust already exposes, mirrored if a JS consumer
needs it. Wire: none.

## Related

- [Drop set](/quest/m1/drop-set.md) - bills its track and cursor runs to this pool
