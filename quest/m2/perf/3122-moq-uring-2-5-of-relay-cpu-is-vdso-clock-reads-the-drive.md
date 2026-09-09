# [M] moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()

## Goal

A worker drive turn reads the clock once and hands that instant down, so
`[vdso]` falls from its ~2.5% share of io_uring relay CPU to the tokio path's
~0.7%, with the QUIC timeout and keep-alive arming unchanged in effect.

## Plan

Profiling the io_uring relay (`dev` @ `fc57e0175`, `perf record -F 499`,
relay process only) shows `[vdso]` as a top-5 DSO, at roughly 3x its share on
the tokio worker path:

| DSO | video, io_uring | video, tokio workers | chat, io_uring |
|---|---|---|---|
| `moq-relay` | 65.89% | 68.61% | 68.96% |
| `[kernel.kallsyms]` | 22.64% | 22.97% | 20.49% |
| `libc.so.6` | 5.74% | 4.48% | 6.03% |
| **`[vdso]`** | **2.95%** | **0.72%** | **2.55%** |

That is `clock_gettime`. Roughly 2.5% of relay CPU spent reading the clock.
The profile is the quiche flavor; re-measure on the default backend (noq
through the `quinn/` module) before and after.

Where the reads are:

- The drive loop reads once per turn to fire timers
  (`self.shared.timers.borrow_mut().fire(Instant::now())`,
  rs/moq-uring/src/worker.rs:183).
- The quiche driver reads again on the same turn in `arm_keep_alive`
  (rs/moq-uring/src/quic/quiche/connection.rs:850-855), plus quiche's own
  `Instant::now()` inside `on_timeout` / `timeout`.
- The default backend has no `arm_keep_alive`; it reads the clock for
  `close` (rs/moq-uring/src/quic/quinn/connection.rs:239), `handle_timeout`
  (:671), and `poll_transmit` (:786). The last one runs once per GSO train,
  since `flush` stages one train per turn (see
  [Egress requeue](/quest/m2/perf/egress-requeue.md)).

The same profile shows the timer heap at ~1.6%:
`<moq_uring::timer::Timer as moq_net::runtime::Timer>::set` 0.92% plus
`btree::search::search_tree` 0.65%. `timer::Heap` is a
`BTreeMap<(Instant, u64), Rc<Slot>>` (rs/moq-uring/src/timer.rs:19-20), so
every QUIC timeout re-arm is an O(log n) map removal and insertion with `Rc`
traffic. The #2875 design note called for a timer wheel. Not urgent at these
connection counts, but it is on the same hot path and grows with it.

`moq_net::runtime::Runtime::now` (rs/moq-net/src/runtime.rs:128) is the
natural place to hand the current turn's instant down instead of having each
layer re-read it. Sample once per drive turn and pass it through `fire`, the
keep-alive arming, `handle_timeout`, `poll_transmit`, and the quiche timeout
calls.

Acceptance: `[vdso]` share in the `perf` profile on both flavors, relay CPU
via `just bench BASE` on Linux, and the existing keep-alive and idle-timeout
tests unchanged.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Closes

- [#3122](https://github.com/moq-dev/moq/issues/3122) - close this issue when the quest finishes
