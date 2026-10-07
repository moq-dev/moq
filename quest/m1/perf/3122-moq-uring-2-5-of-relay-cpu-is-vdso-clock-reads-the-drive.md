# [M] moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()

## Goal

A worker drive turn reads the clock once and hands that instant down, so
`[vdso]` falls from its ~2.5% share of io_uring relay CPU to the tokio path's
~0.7%, with the QUIC timeout and keep-alive arming unchanged in effect.

## Plan

Profiling the io_uring relay (`fc57e0175`, `perf record -F 499`,
relay process only) shows `[vdso]` as a top-5 DSO, at roughly 3x its share on
the tokio worker path:

| DSO | video, io_uring | video, tokio workers | chat, io_uring |
|---|---|---|---|
| `moq-relay` | 65.89% | 68.61% | 68.96% |
| `[kernel.kallsyms]` | 22.64% | 22.97% | 20.49% |
| `libc.so.6` | 5.74% | 4.48% | 6.03% |
| **`[vdso]`** | **2.95%** | **0.72%** | **2.55%** |

That is `clock_gettime`. Roughly 2.5% of relay CPU spent reading the clock.
The profile is the since-deleted quiche driver's; re-measure on noq before
and after. The closed, unmerged prototype
[#3136](https://github.com/moq-dev/moq/pull/3136) froze the clock per turn
behind an RAII guard on that driver; its shape and tests are a starting
point.

Where the reads are:

- The drive loop reads once per turn to fire timers
  (`self.shared.timers.borrow_mut().fire(Instant::now())`,
  rs/moq-uring/src/worker.rs:183).
- The noq driver reads the clock for
  `close` (rs/moq-uring/src/quic/noq/connection.rs:239), `handle_timeout`
  (:671), and `poll_transmit` (:786). The last one runs once per GSO train,
  since `flush` stages one train per turn (see
  [Run to quiescence](/quest/m1/perf/uring-quiescence.md)).

The same profile shows the timer heap at ~1.6%:
`<moq_uring::timer::Timer as moq_net::runtime::Timer>::set` 0.92% plus
`btree::search::search_tree` 0.65%. `timer::Heap` is a
`BTreeMap<(Instant, u64), Rc<Slot>>` (rs/moq-uring/src/timer.rs:19-20), so
every QUIC timeout re-arm is an O(log n) map removal and insertion with `Rc`
traffic. The #2875 design note called for a timer wheel. Not urgent at these
connection counts, but it is on the same hot path and grows with it.

moq-net's drivers already receive the current instant from their owner
(`Clock::now`, "the latest instant supplied by the owning driver", in
`rs/moq-net/src/time.rs`); moq-uring's worker passes `Instant::now()` to its
single `driver.poll(Instant::now(), ...)` call once per poll
(`rs/moq-uring/src/worker.rs`). Sample once per drive turn there and pass
that instant through `fire`, the `handle_timeout`, and `poll_transmit`.

Decided in the 2026-10-05 audit: this edits the QUIC connection driver the
[hard fork](/quest/m1/quic/fork/README.md)'s switch renames and moves onto
`moq-quic`, so it waits for the fork and is measured there.

Acceptance: `[vdso]` share in the `perf` profile on both flavors, relay CPU
via `just bench BASE` on Linux, and the existing keep-alive and idle-timeout
tests unchanged.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the driver this edits moves onto `moq-quic`

## Closes

- [#3122](https://github.com/moq-dev/moq/issues/3122) - close this issue when the quest finishes
