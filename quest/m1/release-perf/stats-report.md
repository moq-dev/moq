# [S] Stats report

## Goal

`stats::Registry::report` takes each row's lock once and reads live sessions
without a compare-and-swap. The `stats/session-report` bench registers
transport `Wire`s, so the per-connection wire walk shows up in the measured
drain. The remaining O(live sessions) cost is accepted, and the reason is
recorded in the [epic](/quest/m1/release-perf/README.md).

## Plan

The source work is complete as of 2026-10-22. `SessionCounters::snapshot`
returns its empty-live condition with the payload totals from one row guard.
`SessionBytes::read` checks the strong count without a CAS and pairs a final
owner's relaxed counter reads with the writers' Arc drops using an Acquire
fence. `SessionTraffic` is private and has no weak references, so a unique
arrival cannot gain a new writer. Per-label bytes, retags, outliving meters,
wire attribution and retired lifetime totals remain unchanged.

`stats/session-report` registers real transport wires, verifies their totals,
and independently sweeps 1/100/1000 labels, 1/10 sessions per label, and
0/1/4 connections per session. Three alternating before/after rounds used
20 samples, 0.3 s warmup and 1 s measurement per case. These are medians of
three per-run medians, in us per report at 1000 labels:

| Sessions per label | Connections per session | Before | After |
|---|---|---|---|
| 1 | 0 | 35.2 | 29.7 |
| 1 | 1 | 66.4 | 60.6 |
| 1 | 4 | 159.6 | 154.3 |
| 10 | 0 | 88.6 | 52.2 |
| 10 | 1 | 416.1 | 388.4 |
| 10 | 4 | 1361.4 | 1335.5 |

The remaining O(live sessions and connections) cost pays for per-label
payload bytes and live transport sampling. At 10,000 sessions with one
connection each, a once-per-second drain uses about 0.04% of a core. The
previous under-0.01% estimate excluded the wire walk. Labels and per-label
payload bytes stay, as already decided in the epic. An O(labels) push design
would move cost to the write path and still needs separate planning.

The implementation is complete, but automatic approval review rejected
`quest rm quest/m1/release-perf/stats-report.md`, treating completion retirement
as abandonment under the unattended instruction. Retain this file and its
links until the maintainer approves that narrow completion cleanup. No
abandonment or larger stats design decision is requested.

Public API: none. Wire: none.
