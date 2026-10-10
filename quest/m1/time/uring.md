# [M] moq-uring on moq-time

## Goal

moq-uring's worker is a `moq-time` backend. It reads the clock once per drive
turn and hands that instant to `fire`, the QUIC `handle_timeout`, and
`poll_transmit`, so `[vdso]` falls from its ~2.5% share of io_uring relay CPU
to the tokio path's ~0.7%, with QUIC timeout and keep-alive arming unchanged
in effect. moq-time's deadline queue replaces `timer::Heap`. The worker tests
`deadline_fires_at_park` and `dropped_worker_rejects_operations` run on
controlled time or event assertions and pass under load.

## Plan

Absorbed [#3122](https://github.com/moq-dev/moq/issues/3122) on 2026-10-10:
its profile (`[vdso]` 2.95% video and 2.55% chat on io_uring against 0.72% on
tokio workers; `Timer::set` plus btree search ~1.6%) and its plan (sample once
per turn; closed prototype #3136 froze the clock per turn behind an RAII
guard) carry over. The per-turn sample is the worker's ambient `now()`; code
on the worker never reads the host clock itself. Use a timer wheel instead of
the queue only if the bench says so. Also absorbed the two worker tests from
[moq-uring tests under load](/quest/m1/uring-tests-under-load.md).

Measure before and after on `moq-quic`: the `[vdso]` share in `perf` on both
flavors, and relay CPU via `just bench BASE` on Linux. The existing keep-alive
and idle-timeout tests stay unchanged. Ban direct clock reads in moq-uring
outside its backend (the ratchet).

Public API: none expected. Wire: none.

## Required

- [moq-net on moq-time](/quest/m1/time/net.md) - the driver's instant becomes moq-time's
- [Switch](/quest/m1/quic/fork/switch.md) - the QUIC driver this edits moves onto `moq-quic`

## Closes

- [#3122](https://github.com/moq-dev/moq/issues/3122) - close this issue when the quest finishes

## Related

- [io_uring handshake deadline](/quest/m1/listener-deadlines.md) - arms its deadline on the worker's clock
- [Run to quiescence](/quest/m1/perf/uring-quiescence.md) - reshapes the same worker turn
