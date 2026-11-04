# [M] A read moves a small group consumer

## Goal

`moq_net::group::Consumer` shrinks from 280 bytes by boxing what a read rarely touches, so handing a group to each reader stops copying 280 bytes through every layer it passes. `just bench-fanout` shows the drop, and the common read path gains no allocation per group.

## Plan

Decided 2026-11-03 (maintainer, on [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605)): a follow-up in this epic.

Found by [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605)'s session scenario (a publisher, a relay origin, and viewers over the in-memory transport): `memcpy` is its largest self cost, about 9.4k of 113k instructions per delivered group at 64 viewers, mostly moves of `group::Consumer` and the `Peeked` that carries one, by value through `Poll<Result<Option<_>>>` layers. The largest callers per group: `track::Subscriber::poll_peek_group` (982), the viewer's `lite::subscriber::UniServe::poll_serve` (715), `track::Subscriber::take` (566, 442, and 428 across its inlined copies), `track::Cursor::poll_peek` (507), and `kio::Tasks<UniServe>::insert` (454).

- Find which fields a read touches per group and box the rest, or the rarely-set state, behind one pointer. Check `size_of` before and after.
- Measure with `just bench-fanout "<base>" "model session" "64 512"`, and `BENCH_FANOUT_CALLGRIND=1` with `bench/fanout.py compare` to see the moves shrink. Keep the session scenario's allocations per group flat: the [relay allocations](/quest/m2/perf/relay-allocs.md) quest tracks them.

Public API: `group::Consumer`'s layout only; no signature change expected. Wire: none.

## Related

- [Relay CPU per frame since #5926](/quest/m1/release-perf/relay-cpu-bisect.md) - found this
- [The relay allocates less per delivered frame](/quest/m2/perf/relay-allocs.md) - boxing must not add an allocation per delivered group
