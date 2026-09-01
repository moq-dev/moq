# [M] Shard the cache pool counters and coarsen its clock

## Goal

The relay creates one `cache::Pool` for the whole process, and every frame
written or read on every worker does relaxed `fetch_add`s on the same cache
line (`Inner::used`, and `Inner::access_sum`/`access_count` for the
evictable population) plus a real `Instant::now()` read through
`Charge::touch` and `Pool::stamp`. At fanout this is the top cross-core
contention point in the model layer. Make per-frame accounting land on
per-owner state and hit the shared line only at a coarse cadence, and stop
reading the clock per frame.

## Plan

Mechanism, per the 2026-09 survey:

- `Charge::add` bumps the process-global `used`, the per-track `written`,
  and stamps `last` via `touch`, which calls `Pool::stamp` and therefore
  `model::clock::now()` per frame, plus an `access_sum` `fetch_add` when
  the group is in the evictable population.
- The precedent already exists in the same file: `cache::Track` accumulates
  `written` per track and drains into eviction debt only at a 256 KiB
  threshold or the expiry-scan tick. The pool's own module docs argue for
  lock-free distributed reclamation with convergence, not exactness.

Proposal:

- Extend the `cache::Track` accumulator pattern so `used` moves in batched
  deltas: per-owner (track or thread-local shard) counters that flush to
  the global on a byte threshold, keeping the global a converging
  approximation the governor already tolerates.
- Same treatment for `access_sum`/`access_count`: the access average only
  steers eviction, so per-owner accumulation with periodic reconciliation
  is enough.
- Replace the per-frame clock read with a coarse tick: a pool epoch
  advanced by the drive loop or a cheap thread-local cached instant with
  bounded staleness. Coordinate the seam with
  [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md),
  which threads a per-turn timestamp through `moq_net::runtime`; the model
  layer should drink from the same cup rather than grow a second clock.
- Eviction correctness bounds the design: staleness must never let the pool
  exceed its ceiling by more than the sum of unflushed deltas, and that
  bound must be stated and tested.

Acceptance: cross-core traffic on the pool line (perf stat cycles or c2c
where available), relay CPU and RSS at the fanout shape via
`just bench BASE` on Linux. Eviction behavior covered by the existing cache
tests plus new ones for the staleness bound.

## Related

- [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - the same clock reads one layer down
