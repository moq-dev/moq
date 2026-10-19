# [M] moq-uring on moq-clock

## Goal

moq-uring's worker is a `moq-clock` backend. It reads the clock once per drive
turn and hands that instant to `fire`, the QUIC `handle_timeout`, and
`poll_transmit`, so `[vdso]` falls from its ~2.5% share of io_uring relay CPU
to the tokio path's ~0.7%, with QUIC timeout and keep-alive arming unchanged
in effect. moq-clock's deadline queue replaces `timer::Heap`. The worker tests
`deadline_fires_at_park` and `dropped_worker_rejects_operations` run on
controlled time or event assertions and pass under load.

## Plan

Absorbed [#3122](https://github.com/moq-dev/moq/issues/3122) on 2026-10-10:
its profile (`[vdso]` 2.95% video and 2.55% chat on io_uring against 0.72% on
tokio workers; `Timer::set` plus btree search ~1.6%) and its plan (sample once
per turn; closed prototype #3136 froze the clock per turn behind an RAII
guard) carry over. The per-turn sample is the worker's ambient `now()`; code
on the worker never reads the host clock itself. Use a timer wheel instead of
the queue only if the bench says so.

Measure before and after on `moq-quic`: the `[vdso]` share in `perf` on both
flavors, and relay CPU via `just bench BASE` on Linux. The existing keep-alive
and idle-timeout tests stay unchanged. Ban direct clock reads in moq-uring
outside its backend (the ratchet).

Move `uring_workers_bound_stalled_handshakes` (`rs/moq-relay/tests/integration/runtime_uring.rs`)
onto controlled time too; it landed on the wall clock in #5244 rather than
waiting for this quest.

Public API: breaking. `moq_uring::Timer` is re-exported, and `Timer::set`
takes `Option<moq_clock::Instant>` instead of std's; no conversion shim.
Wire: none.

## Closes

- [#3122](https://github.com/moq-dev/moq/issues/3122) - close this issue when the quest finishes

## Related

- [Run to quiescence](/quest/m2/perf/uring-quiescence.md) - reshapes the same worker turn
