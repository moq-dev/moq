# [M] Probing beside Cubic meets the deadline budget

## Goal

`tests::probe::competing_cubic` in `rs/moq-quic/src/tests/probe.rs`, run by
Nightly through `just rs quic-sim`, passes its deadline and p99 budgets on
`main`, fixed at its cause.

## Plan

[moq-dev/moq#6942](https://github.com/moq-dev/moq/pull/6942) fixed the probe's
own late frames. Measured over seeds 1-1024 since then, probing adds no late
frames (−4.4 ± 2.8 against a 4.5 budget) but costs 58.7 ± 4.3 ms of p99 against
a 40 ms allowance. Seeds 1-8 still fail on late frames (451.0 against 385.8).

Both failures come from the probe's first duplicates: about 7 kB, five
packets, in the first 0.4 s. It sends nothing more from 1 s to 11 s.
Without Startup probing, the paired difference over 64 seeds is +2.1 ± 1.0 late
frames and +1.0 ± 0.7 ms of p99, and the per-seed late spread drops from 89
frames to 8.

- **The Startup duplicates re-randomize the run.** They make Startup rounds
  count toward full bandwidth, which an application-limited baseline skips.
  So BBR3 leaves Startup earlier (1.31 s against 1.41 s on the mean), and every
  later ProbeBW cycle shifts against Cubic's. Each run then lands near either
  320 or 425 late frames independently of its baseline. With that spread,
  resolving the 4.5-frame budget to two standard errors takes about 1600 seeds.
- **Leaving Startup earlier costs p99 beside a filling queue.** The estimate
  collapses beside Cubic once the Startup sample leaves the max-bw window
  (4.51 s against 4.68 s) and stays down until the 40 ms min RTT expires at
  about 9.7 s. Until then a cwnd of 2 BDP at 40 ms carries about 0.4 of what a
  190 ms round trip needs. Making the baseline count its application-limited
  Startup rounds too, so it leaves Startup the way a probe-filled sender does,
  raises its own p99 by 220 ± 4 ms over 1024 seeds. The cost belongs to BBR3
  leaving Startup on time beside a standing queue; the baseline avoids it by
  staying in Startup.

Tried, and rejected:

- Filling Startup to a 2.0 gain instead of ProbeBW_UP's 1.25: +65 ms p99,
  +29 late frames.
- Holding the Startup probe until Startup has seen one application-limited
  round: +125 ms p99.
- Holding it until three such rounds: competing_cubic costs +0.2 ms p99 and
  +2.0 ± 0.3 late frames, and the suite passes on seeds 1-8. But a
  constant-bitrate sender then stays in Startup about 10 s, and its rise after
  the 20 s step grows from 0.3 s to 4.1 s in `steps_constant` and
  `credit_limited` (the budget is 5 s).

Each scenario run now traces at INFO instead of TRACE. That cuts the probe suite
from 11.5 s to 2.7 s, and a failing case no longer dumps 330 MB of packet trace.
128 seeds of competing_cubic take 12 s on one thread.

The remaining work depends on how the p99 cost is resolved, which is open in
this quest's PR.

Do not relax the assertions past what the measured spread supports, or disable
probing, to make the scenario pass.
Startup probing stays: without it a constant-bitrate sender never leaves
Startup.

Run the probe suite with release debug assertions and overflow checks, as
`just rs quic-sim` does. `just rs test --release -p moq-quic --run-ignored all
--no-fail-fast -E 'test(/^tests::probe::/)'` also includes the short gate cases.

Public API: none. Wire: none.
