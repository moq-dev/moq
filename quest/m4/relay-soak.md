# [M] A nightly soak shows a churning relay stops growing

## Goal

Nightly runs `moq-relay` as a real process under sustained churn from
`moq-bench` (sessions connecting and leaving, broadcasts starting and ending,
subscribers joining late and stalling) and fails when resident memory, open
file descriptors, or live tasks keep growing after warm-up. The failure
names which one grew and attaches its samples.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): nothing runs a relay for long. The
[simulated soak](/quest/m4/cluster-fault-schedules.md) catches a table that
never shrinks; this catches what it cannot see: the allocator, descriptors,
real sockets, and io_uring.

- ✅ Both soaks (rejected: sim only; real process only).
- ✅ m4 (rejected: m1).

Agent's choices, for the implementer to revisit:

- The run's length is measured, not picked: seed a known slow leak with a
  patch (one per resource, kept as `test/drill/mutations` keeps its patches)
  and take the shortest run that flags each reliably while a clean relay
  passes. Report the numbers.
- Judge the slope after warm-up over the whole run, never a single sample or
  an absolute ceiling, which would track the host.
- Cover both backends the relay ships, tokio and io_uring.
- It never gates a merge, as benchmarks do not (2026-10-20, maintainer).

Public API: none. Wire: none.

## Hosts

- linux - io_uring and `/proc` sampling

## Related

- [Relay allocations](/quest/m4/perf/relay-allocs.md) - steady-state allocation cost, where this measures growth
