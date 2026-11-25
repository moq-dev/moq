# [S] Reserving N harness ports costs time linear in N

## Goal

A run that reserves N ports through `harness_port` (`test/lib/harness.sh`) spends
time linear in N, and several large runs fit on one host at once. Today each call
scans up from `HARNESS_PORT_BASE` (4500), so the k-th port of a run first retries
the k-1 it already holds, each retry costing a `flock` plus `test/lib/reserve.sh`'s
`ps` calls: quadratic in N. A 32-relay mesh replay's 162 ports took 13 to 20
minutes on loaded hosts, and the 501-port window fits only three such replays at
once.

## Plan

Recommended: start each search after the port this run last reserved, wrapping
to `HARNESS_PORT_BASE` once past the window's end, so a run never re-checks its
own ports yet still finds a free one below its last (as after an explicit
`wanted` near the top). Let a run that needs many ports ask for them in one call
(`harness_port` taking a count, or a sibling that fills an array), so the lock
and `reserve.sh` run once per port probed rather than once per port held. Widen
the window too, since wrapping alone leaves the fixed 501 ports as the limit on
concurrent runs: a few thousand ports fit many 162-port replays. Keep the
reservation semantics `harness.sh` documents: held for the run, claimed
atomically under the advisory lock, and a stale owner detected by its process
identity.

Callers include `test/mesh-replay/run.sh`, `test/drain/run.sh`, and
`test/interop/interop.sh`; `grep -rn harness_port test bench` lists the rest. An
explicit `wanted` port keeps its current behavior.

Verify with `test/lib/harness.test.sh`: a case that reserves a few hundred ports
and counts `reserve.sh` invocations (linear, not quadratic), and one where two
runs reserve concurrently and never share a port. Time a 32-relay replay's port
reservation before and after.

Decided 2026-11-25 (maintainer, `/quest-plan`): ✅ keep (rejected: drop).
