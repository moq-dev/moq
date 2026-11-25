# [S] Make the relay's concurrent-groups sim test fast

## Goal

`quic_sim::concurrent_groups_carry_bulk_transfers_losslessly`
(`rs/moq-relay/tests/integration/quic_sim.rs`) finishes well inside nextest's
SLOW mark, so `just check` never times it out.

## Plan

On a loaded host it takes 40 to 60 s on `main` (5589d3c295: 41, 45, 45, 51,
and 59 s), against nextest's 60 s limit, and timed out once in a full
`just check` on
[moq-dev/moq#9518](https://github.com/moq-dev/moq/pull/9518), whose branch
runs it in the same 39 to 52 s. The test runs on the simulated clock, so the
time is CPU spent simulating, not waiting. Profile one run (`perf record` on
the nextest binary) to see what dominates, such as per-packet work in the
simulated network or the transfer size, then shrink what the test does
without losing what it checks: concurrent groups carried losslessly under
the sim's congestion. Do not raise the timeout: a SLOW test is a bug to fix.
