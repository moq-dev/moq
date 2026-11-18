# [M] The browser resume-history control releases its last route

## Goal

The browser bench smoke's `resume-history` microbenchmark stops failing its control
intermittently on Nightly: once every front concludes and the serving route ends,
nothing of that route stays reachable. The cause is found and fixed at the root, with
a deterministic test that fails without the fix.

## Plan

Seen on Nightly runs
[43601796176](https://github.com/moq-dev/moq/actions/runs/43601796176) (2026-11-14,
"8 fronts, 1 names, 2 routes, churn, subscribe") and
[44127032469](https://github.com/moq-dev/moq/actions/runs/44127032469) (2026-11-17,
"8 fronts, 1 names, 2 routes, stable, fetch"), each with "the control kept
{"sessions":1,"transports":1,"broadcasts":8,"bytes":0} after concluding": the serving
route's client session, its transport, and its 8 broadcast handles. The 11-15 and
11-16 runs passed. Split out of the lite-07 route hang quest: the bench speaks
lite-06 (`ALPN_06`) and shares no cause with the lite-07 hangs.

It did not reproduce locally: three runs of
`bun micro.ts --bench resume-history --rounds 4 current=../.. aa=../..` in Chrome,
about 36 full sweeps, all passed. `drain` in `js/net/bench/resume-history.ts` returns
once two consecutive collections count the same, so teardown still pending on a host
task or a real-time timer after `conclude` would read as kept on a slower runner.
Trace what still references the client session after `conclude` (a heap snapshot
from the failing case) before changing the harness.
