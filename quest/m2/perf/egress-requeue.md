# [XS] Trains per turn on the egress driver becomes a measured budget

## Goal

The io_uring QUIC driver stages one GSO train per turn and then wakes
itself (`Driver::flush`, rs/moq-uring/src/quic/noq/connection.rs:744), so a
deep backlog on one connection pays a whole driver turn per train of
`TRAIN_SEGMENTS = 63` segments (noq/connection.rs:23; `MAX_GSO_SEGMENTS =
64` at udp.rs:56 is the kernel cap). That cadence is a hardcoded fairness choice.
Make it a measured budget.

## Plan

The re-walk half of #3120 landed in #3134 (e6962b20e) on the since-deleted
quiche driver; the noq driver drains an event queue instead of walking
iterators (noq/connection.rs:697) and never had that shape. What is left is
the budget.

- Add a trains-per-turn budget to `flush`. One train then
  requeue is deliberate fairness across connections sharing a socket; keep
  fairness by bounding the budget, and sweep 1, 2, and 4 trains per turn
  under the fanout and single-heavy-connection shapes to see whether the
  extra turn latency is real.
- Re-profile on noq; the numbers in #3120 are the quiche driver's.

Acceptance: CPU per Gbps and throughput ceiling via `just bench BASE` on
Linux. Latency must not regress at the chosen budget. A no-win keeps 1.

The [quiescence quest](/quest/m2/perf/uring-quiescence.md) sweeps this
budget together with its pass budget; land whichever runs first and fold the
other's sweep in.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Closes

- [#3120](https://github.com/moq-dev/moq/issues/3120) - close this issue when the quest finishes
