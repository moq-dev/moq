# [S] Probing beside Cubic meets the deadline budget

## Goal

`tests::probe::competing_cubic` in `rs/moq-quic/src/tests/probe.rs`, run by
Nightly through `just rs quic-sim`, passes its deadline budget on `main`,
fixed at its cause.

## Plan

Over seeds 1 through 8, probing beside a Cubic upload averages 413.6 late
frames versus 385.1 for the baseline, beyond the `LATE` budget. Find why
probing adds delay beside Cubic and fix it in the transport. Keep the
overhead, fairness, and rise budgets and the other probe scenarios passing.
Do not relax the assertions or disable probing to make the scenario pass.

Run the probe suite with release debug assertions and overflow checks, as
`just rs quic-sim` does. `just rs test --release -p moq-quic --run-ignored all
--no-fail-fast -E 'test(/^tests::probe::/)'` also includes the short gate cases.

Public API: none. Wire: none.
