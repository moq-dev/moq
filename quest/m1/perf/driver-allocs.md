# [S] Stop allocating per train and per pump in the egress driver

## Goal

Three steady-state costs in moq-uring's egress and completion loops:

- `TxBuf::send` heap-allocates a `Box<SendHdr>` (plus an `Rc<TxLease>` and
  a slab insert) for every `sendmsg` train.
- `pump_inner` allocates a fresh `Vec<Cqe>` for every completion batch.
- `Driver::flush` stages one GSO train (up to 63 segments) per worker turn
  and then self-wakes, so a connection with a deep backlog needs one full
  loop turn per train.

Kill the per-train and per-pump allocations, and turn the one-train-per-turn
cadence into a measured knob instead of a hardcoded fairness choice.

## Plan

- Pool the `SendHdr` allocations alongside the TX buffer leases they
  describe: the pool already owns stable per-worker buffers, so the header
  block can live with the buffer slot and be reused.
- Keep a persistent CQE scratch buffer on the worker, drained per pump,
  instead of allocating per batch.
- Add a trains-per-turn budget to `Driver::flush`. The current
  one-train-then-requeue shape is deliberate fairness across connections on
  the shared socket; keep fairness by bounding the budget, and sweep 1, 2,
  and 4 trains per turn under the fanout and single-heavy-connection shapes
  to see whether the extra turn latency is real.
- Each change stays independently ablatable.

Acceptance: allocations per train and per pump (heaptrack or a debug
counter), CPU per Gbps and throughput ceiling via `just bench BASE` on
Linux. Latency must not regress at the swept budget.
