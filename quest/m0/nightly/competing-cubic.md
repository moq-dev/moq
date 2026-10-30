# [S] Probing beside Cubic meets the deadline budget

## Goal

`tests::probe::competing_cubic` in `rs/moq-quic/src/tests/probe.rs`, run by
Nightly through `just rs quic-sim`, passes its deadline and p99 budgets on
`main`, fixed at its cause.

## Plan

[moq-dev/moq#6942](https://github.com/moq-dev/moq/pull/6942) fixed the probe's
own late frames. Measured over seeds 1-128, probing adds 1.9 late frames over the
baseline (the budget is 4.5), where `main` before it added 31. It landed with
the scenario still failing Nightly on seeds 1-8. Two problems remain, and this
quest owns both:

- **Eight seeds cannot resolve the late budget.** Beside Cubic, each run lands
  near either 320 or 425 late frames, so the paired difference over 8 seeds has
  a standard error of about 30 frames against a budget of 4.5. Seeds 1-8 read
  451.0 against 385.8 even though seeds 1-128 agree within the budget.
- **Startup probing costs about 54 ms of p99.** Over seeds 1-128 the p99 is
  54 ± 11 ms above the baseline's, against a 40 ms allowance. Probing only in
  Startup costs the same, and probing only after Startup costs 1 ms. The
  Startup duplicates make BBR3 count full-bandwidth rounds, so it leaves
  Startup about 0.2 s earlier, and the 4-10 s collapse beside Cubic's queue
  then goes deeper. Since
  [moq-dev/moq#7017](https://github.com/moq-dev/moq/pull/7017) aged an
  application-limited estimate, that collapse has raised the baseline's own
  p99 from about 1000 ms to 1390 ms.

Remaining work:

1. Re-size the scenario on more seeds. Run it over enough seeds (64 take about
   30 s in release) that the paired late-frame difference resolves its budget,
   and size the late and p99 budgets from the measured spread.
2. Chase the Startup p99 cost until probing beside Cubic stays within the p99
   allowance, starting from the earlier Startup exit and the collapse beside a
   standing queue that #7017 introduced.

Do not relax the assertions past what the measured spread supports, or disable
probing, to make the scenario pass.
Startup probing stays: without it a constant-bitrate sender never leaves
Startup.

Run the probe suite with release debug assertions and overflow checks, as
`just rs quic-sim` does. `just rs test --release -p moq-quic --run-ignored all
--no-fail-fast -E 'test(/^tests::probe::/)'` also includes the short gate cases.

Public API: none. Wire: none.
