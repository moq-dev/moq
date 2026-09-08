# [S] The egress driver's requeue stops re-walking every ready stream

## Goal

Sending a deep backlog on one connection no longer re-runs the whole of
`Driver::poll` per GSO train. The driver separates "there is more to send"
from "readiness changed", so a requeue for the transmit pool walks nothing,
and the one-train-per-turn cadence becomes a measured budget instead of a
hardcoded fairness choice.

## Plan

Branch from dev. Profiled in #3120: `kio::waiter::WaiterList::register` is
the hottest symbol on the io_uring relay (4.6% video, 6.7% chat), because
`Connection::flush` stages one train (up to 63 segments) and then wakes
itself, and each of those turns walks quiche's `readable()` and `writable()`
iterators, removing and re-registering a waiter per ready stream. The same
sweep calls `stream_capacity` for every finishing stream per turn.

- Separate the requeue from readiness. After a train, the driver asks only
  for another transmit turn; the readiness walk runs when quiche reports a
  readiness change, or only for the streams whose readiness changed since
  the last turn. `state.finishing.retain(..)` stops calling
  `stream_capacity` per finishing stream per turn.
- Add a trains-per-turn budget to `Driver::flush`. The one-train-then-requeue
  shape is deliberate fairness across connections on the shared socket; keep
  fairness by bounding the budget, and sweep 1, 2, and 4 trains per turn
  under the fanout and single-heavy-connection shapes to see whether the
  extra turn latency is real.
- Both changes stay independently ablatable, and the quinn driver gets the
  same treatment where it has the same shape.

Acceptance: `WaiterList::register` share in the `perf` profile, CPU per Gbps
and throughput ceiling via `just bench BASE` on Linux. Latency must not
regress at the swept budget.

## Closes

- [#3120](https://github.com/moq-dev/moq/issues/3120) - close this issue when the quest finishes
