# [XS] Trains per turn on the egress driver becomes a measured budget

## Goal

Both io_uring QUIC drivers stage one GSO train per turn and then wake
themselves (`Driver::flush`, rs/moq-uring/src/quic/quiche/connection.rs:780-790
and quinn/connection.rs:744), so a deep backlog on one connection pays a whole
driver turn per train of `TRAIN_SEGMENTS = 63` segments
(quiche/connection.rs:20, quinn/connection.rs:23; `MAX_GSO_SEGMENTS = 64` at
udp.rs:56 is the kernel cap). That cadence is a hardcoded fairness choice.
Make it a measured budget.

## Plan

The re-walk half of #3120 landed in #3134 (e6962b20e): the quiche driver only
sweeps readiness after ingress (`Sweep::after_ingress`,
quiche/connection.rs:627-661, regression
`transmit_continuation_skips_event_sweep` :918). The quinn driver drains an
event queue instead of walking iterators (quinn/connection.rs:697) and never
had that shape. What is left is the budget.

- Add a trains-per-turn budget to `flush` on both backends. One train then
  requeue is deliberate fairness across connections sharing a socket; keep
  fairness by bounding the budget, and sweep 1, 2, and 4 trains per turn
  under the fanout and single-heavy-connection shapes to see whether the
  extra turn latency is real.
- Profile the default backend (noq through the `quinn/` module) and the
  quiche flavor separately; the numbers in #3120 are quiche's.

Acceptance: CPU per Gbps and throughput ceiling via `just bench BASE` on
Linux. Latency must not regress at the chosen budget. A no-win keeps 1.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Closes

- [#3120](https://github.com/moq-dev/moq/issues/3120) - close this issue when the quest finishes
