# [S] Probe against Cubic passes

## Goal

`tests::probe::competing_cubic` in `rs/moq-quic/src/tests/probe.rs`, run by
Nightly through `just rs quic-sim`, passes on `main`, fixed at its cause.

## Plan

The expiry helper now records only actual resets. It previously marked a
completed stream as reset at its group's end time, falsely rejecting probe
packets sent before completion and still in flight. The reset assertion and
acceptance budgets remain unchanged.

With that bookkeeping fixed, the release suite reaches a separate failure:
over seeds 1 through 8, probing averages 413.6 late frames versus 385.1 for
the baseline, beyond the existing deadline budget. Investigate why probing
beside Cubic adds delay and fix it at its source, preserving the overhead,
fairness, and rise budgets and the other probe scenarios. Do not relax the
assertions or disable probing to make the scenario pass.

Run the probe suite with release debug assertions and overflow checks, as
`just rs quic-sim` does. `just rs test --release -p moq-quic --run-ignored all
--no-fail-fast -E 'test(/^tests::probe::/)'` also includes the short gate cases.

Public API: none. Wire: none.
