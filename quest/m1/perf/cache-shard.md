# [M] Batch the cache pool counters

## Goal

The relay creates one `cache::Pool` for the whole process, and every frame
written or read on every worker does relaxed `fetch_add`s on the same cache
line (`Inner::used`, and `Inner::access_sum`/`access_count` for the
evictable population). At fanout this is the top cross-core contention
point in the model layer. Make per-frame accounting land on per-owner state
and hit the shared line only at a coarse cadence.

## Plan

Two pieces of the original survey are settled; one is not.

Settled, and not to be retried as specified: **sharding the counters**.
16-way sharding of the process-wide counters was implemented and measured,
then abandoned. Repeated runs were mixed, and one regressed the fanout
benchmark by 15.6% at 64 readers and 10.5% at 512. It also wanted delayed
accounting, explicit overshoot bounds, and more reconciliation for no
consistent win. Sharding a counter that is already only relaxed
`fetch_add`s trades one cheap contended line for several lines plus a
read-side fold, and the fold is what showed up.

Settled: **the duplicate clock read per frame write**. A write called
`model::clock::now()` twice, once through `Charge::touch` -> `Pool::stamp`
and again through `Track::settle` -> `expiry_due`. The write path now
threads the tick it already sampled into `Track::settle`. That is exact
duplicate removal, not a policy change: the clock is no coarser, nothing is
delayed, and eviction and staleness bounds are untouched.

Open, and the real content of this quest: **batching the counters
themselves**. The precedent is in the same file, where `cache::Track`
accumulates `written` per track and drains into eviction debt only at a
256 KiB threshold or the expiry-scan tick, and the pool's module docs argue
for lock-free distributed reclamation with convergence rather than
exactness.

- Extend that accumulator so `used` moves in batched deltas: per-owner
  counters that flush to the global on a byte threshold, keeping the global
  a converging approximation the governor already tolerates.
- Same for `access_sum`/`access_count`. The access average only steers
  eviction, so per-owner accumulation with periodic reconciliation is
  enough.
- Eviction correctness bounds the design: staleness must never let the pool
  exceed its ceiling by more than the sum of unflushed deltas, and that
  bound must be stated and tested.

Note that this is the half sharding was standing in front of, and it is not
the same idea: batching reduces how *often* the shared line is touched,
where sharding kept the frequency and spread the address. Measure it on its
own before concluding anything from the sharding result.

Acceptance: cross-core traffic on the pool line (perf stat cycles or c2c
where available), relay CPU and RSS at the fanout shape via
`just bench BASE` on Linux, plus `track_parallel_write` in
`rs/moq-net/benches/track.rs` for the writer-side shape. Eviction behavior
covered by the existing cache tests plus new ones for the staleness bound.

## Related

- [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - the remaining clock reads one layer down; a pool epoch driven by its per-turn timestamp is the way to drop the last per-frame read, and the model should drink from that cup rather than grow a second clock
