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

Branch from dev. Keep the fairness the one-pass rule protects: a forward wake
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
- The egress driver's one-train-then-self-wake shape
  (rs/moq-uring/src/quic/noq/connection.rs:743-751) interacts with the
  budget: a deep backlog would consume every pass. Fold the
  [egress requeue](/quest/next/perf/egress-requeue.md) train budget into the
  same sweep so trains per turn and passes per turn are measured together.
- Add `passes` per turn to the metrics beside `turns`.

Acceptance: turns and enters per received datagram on the chat shape
(request-reply is the worst case), CPU per Gbps and p99 latency on the fanout
shape, via `just bench BASE` on Linux. The existing starvation test keeps its
meaning with a budget of 1 and gains a sibling proving the budget bound.

## Required

- [One enter per turn](/quest/next/perf/uring-one-enter.md) - the metrics and
  the submit placement this sweep is measured with

## Related

- [Egress requeue](/quest/next/perf/egress-requeue.md) - the train budget
  measured in the same sweep
