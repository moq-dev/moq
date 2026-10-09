# [M] Run to quiescence before submit

## Goal

A received packet's QUIC processing and the sends it produces are staged in
the same drive-loop turn, so one enter carries the reply. Today
`Tasks::poll` runs one pass over the ready set per turn and a mid-pass wake
lands in the next turn by design (rs/kio/src/task.rs:246-257, test
`a_mid_pass_wake_lands_in_the_next_pass`), so a datagram costs three turns:
dispatch sets the demux bit, the demux drains packets and kicks each
connection driver (rs/moq-uring/src/quic/noq/endpoint.rs:208-220,
:285-290), and the drivers stage `SendMsg` a turn later. Every hop is a turn
and up to an enter. The other io_uring runtimes drain their local queue to
exhaustion, or to a quantum, before touching the ring.

## Plan

The pass budget builds on `kio`'s current `Tasks::poll` (#4156 merged
`Pollable` into `Task`). Keep the fairness the one-pass rule protects: a forward wake
chain must not starve the caller's other arms, and one connection's backlog
must not starve the socket.

- `Tasks::poll` takes a pass budget. It re-snapshots the bitset after each
  pass and runs again while anything is set and the budget holds, so a chain
  of bounded depth completes within the turn. A chain deeper than the budget
  is deferred exactly as today. The budget is a `Worker` setting; sweep 1
  (today), 2, 4, and 8.
- Dispatch runs before the task pass, not after: the loop becomes reap and
  dispatch, then tasks to quiescence, then submit or park. That removes the
  stale first pass a park-returning turn runs today (worker.rs:180 polls on
  readiness the previous turn already consumed).
- The egress driver stages one GSO train of `TRAIN_SEGMENTS = 63` segments
  per turn and then wakes itself (`Driver::flush`,
  rs/moq-uring/src/quic/noq/connection.rs:743-751), so a deep backlog pays a
  whole turn per train and would consume every pass. That cadence is a
  hardcoded fairness choice across connections sharing a socket. Make it a
  trains-per-turn budget on `flush` and sweep 1, 2, and 4 together with the
  pass budget, under the fanout and single-heavy-connection shapes, so trains
  per turn and passes per turn are measured together. A no-win keeps 1 train.
  The #3120 numbers are the deleted quiche driver's; re-profile on noq.
  Decided in the 2026-09-30 audit: the egress requeue quest merged here, since
  both budgets need the same sweep.
- Add `passes` per turn to the metrics beside `turns`.

Acceptance: turns and enters per received datagram on the chat shape
(request-reply is the worst case), CPU per Gbps and p99 latency on the fanout
shape, via `just bench BASE` on Linux. The existing starvation test keeps its
meaning with a budget of 1 and gains a sibling proving the budget bound.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the noq driver this edits moves onto `moq-quic` (decided in the 2026-10-05 audit)
- [One enter per turn](/quest/m1/perf/uring-one-enter.md) - the metrics and
  the submit placement this sweep is measured with

## Related

- [Serve budget](/quest/m0/serve-budget.md) - a per-task budget bounds one task's loop; this quest's pass count bounds passes per turn, and the sweep runs with both in place
